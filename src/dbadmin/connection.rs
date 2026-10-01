//! One WebSocket connection: authentication, then SurrealDB RPC requests
//! answered against the daemon's own database handle.
//!
//! Every connection gets a *clone* of that handle. A clone is a separate
//! session over the same datastore, so a Studio `USE`, `LET` or sign-in changes
//! only this connection's namespace, database and variables and can never move
//! the daemon's own queries to another database.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::ws::{Message, WebSocket};
use surrealdb::Surreal;
use surrealdb::engine::any::Any;
use surrealdb::types::{AuthError, Error as TypesError, NotAllowedError, Value};
use surrealdb_rpc::error::{invalid_params, lq_not_supported, session_exists, session_not_found};
use surrealdb_rpc::{DbResponse, DbResult, Method, QueryResult, QueryType, Request};
use tokio::select;
use tracing::{debug, info, warn};
use uuid::Uuid;

use super::Shared;
use super::wire::Format;

/// Wrong credentials allowed before the connection is dropped. A browser
/// retrying a stale saved token needs a couple of attempts; a guessing script
/// does not get an unbounded number.
const MAX_REFUSED_LOGINS: u8 = 5;

/// Sessions one connection may attach beyond its default one. SurrealDB's
/// newer clients open one per query tab; this bounds what a loop could create.
const MAX_ATTACHED_SESSIONS: usize = 64;

/// How long a failed sign-in is made to wait. Slows a guesser without
/// noticeably affecting a person who mistyped.
const FAILED_SIGNIN_DELAY: Duration = Duration::from_millis(250);

/// What a client may do before it has authenticated: the handshake-time
/// methods every SurrealDB client sends first. `use` only changes this
/// connection's own session, and nothing here reads data.
fn allowed_before_auth(method: Method) -> bool {
    matches!(
        method,
        Method::Ping
            | Method::Version
            | Method::Use
            | Method::Signin
            | Method::Authenticate
            | Method::Invalidate
            | Method::Reset
            | Method::Attach
            | Method::Detach
            | Method::Sessions
    )
}

/// Serve one upgraded WebSocket until the client leaves or the endpoint stops.
///
/// `pre_authenticated` is true when the upgrade request already carried a valid
/// bearer token. Nothing the client sent is ever logged beyond the method name:
/// queries and credentials both travel in the parameters.
pub(super) async fn run(
    mut socket: WebSocket,
    format: Format,
    shared: Arc<Shared>,
    pre_authenticated: bool,
    peer: std::net::SocketAddr,
) {
    let opened = Instant::now();
    info!(%peer, ?format, "database admin connection opened");

    let starts_authenticated = pre_authenticated || shared.auth.is_none();
    let mut connection = Connection {
        default: Session::new(&shared, starts_authenticated),
        attached: HashMap::new(),
        starts_authenticated,
        refused_logins: 0,
        shared: Arc::clone(&shared),
    };

    loop {
        let message = select! {
            () = shared.cancel.cancelled() => {
                let _ = socket.send(Message::Close(None)).await;
                break;
            }
            message = socket.recv() => message,
        };
        let bytes = match message {
            Some(Ok(Message::Text(text))) => text.as_bytes().to_vec(),
            Some(Ok(Message::Binary(bytes))) => bytes.to_vec(),
            // Control frames are answered by the WebSocket layer itself.
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
            Some(Ok(Message::Close(_)) | Err(_)) | None => break,
        };

        let response = match format.decode_request(&bytes) {
            // The shutdown token also interrupts a running query, so stopping
            // the endpoint is not held up by a long `SELECT`.
            Ok(request) => select! {
                () = shared.cancel.cancelled() => break,
                response = connection.respond(request) => response,
            },
            Err(error) => DbResponse::failure(None, None, error),
        };
        match format.encode_response(response) {
            Ok(frame) => {
                if socket.send(frame).await.is_err() {
                    break;
                }
            }
            Err(reason) => {
                warn!(%peer, %reason, "could not encode a database admin response");
                break;
            }
        }
        if connection.refused_logins >= MAX_REFUSED_LOGINS {
            warn!(%peer, "closing a database admin connection after repeated failed sign-ins");
            let _ = socket.send(Message::Close(None)).await;
            break;
        }
    }

    info!(%peer, seconds = opened.elapsed().as_secs(), "database admin connection closed");
}

/// One WebSocket connection's sessions and its sign-in bookkeeping.
struct Connection {
    /// The session requests without a session id run in.
    default: Session,
    /// Sessions the client attached explicitly, by the id it chose. SurrealDB's
    /// newer clients use one per query tab.
    attached: HashMap<Uuid, Session>,
    /// Whether a session attached from now on starts out authenticated.
    starts_authenticated: bool,
    /// Consecutive sign-in or authenticate attempts that were refused, across
    /// every session on the connection: opening a new tab per guess must not
    /// reset the count.
    refused_logins: u8,
    shared: Arc<Shared>,
}

impl Connection {
    /// Answer one request, echoing its id and session back as SurrealDB's
    /// clients match responses to requests by them.
    async fn respond(&mut self, request: Request) -> DbResponse {
        let Request {
            id,
            session_id,
            txn,
            method,
            params,
            ..
        } = request;
        let session = session_id.map(Uuid::from);
        let started = Instant::now();
        let result = if txn.is_some() {
            Err(unsupported(
                method,
                "transactions are not supported by the database admin endpoint; \
                 send a `BEGIN`/`COMMIT` block inside one `query` instead",
            ))
        } else {
            self.dispatch(session, method, params.into_vec()).await
        };
        // Method and outcome only: the parameters hold the query text and any
        // credentials.
        debug!(
            method = method.to_str(),
            ok = result.is_ok(),
            millis = started.elapsed().as_millis(),
            "database admin request"
        );
        DbResponse::new(id, session_id.map(Into::into), result)
    }

    async fn dispatch(
        &mut self,
        key: Option<Uuid>,
        method: Method,
        params: Vec<Value>,
    ) -> Result<DbResult, TypesError> {
        match method {
            Method::Attach => return self.attach(key),
            Method::Detach => return self.detach(key),
            Method::Sessions => return Ok(self.sessions()),
            _ => {}
        }

        let session = match key {
            None => &mut self.default,
            Some(id) => self
                .attached
                .get_mut(&id)
                .ok_or_else(|| session_not_found(id))?,
        };
        let outcome = session.dispatch(&self.shared, method, params).await;
        if matches!(method, Method::Signin | Method::Authenticate) {
            self.refused_logins = match outcome {
                Ok(_) => 0,
                Err(_) => self.refused_logins.saturating_add(1),
            };
        }
        outcome
    }

    /// `attach`: open a session under the id the client chose.
    fn attach(&mut self, key: Option<Uuid>) -> Result<DbResult, TypesError> {
        let Some(id) = key else {
            return Err(invalid_params("Expected a session ID"));
        };
        if self.attached.contains_key(&id) {
            return Err(session_exists(id));
        }
        if self.attached.len() >= MAX_ATTACHED_SESSIONS {
            return Err(invalid_params(format!(
                "too many sessions on one connection (the limit is {MAX_ATTACHED_SESSIONS})"
            )));
        }
        self.attached
            .insert(id, Session::new(&self.shared, self.starts_authenticated));
        Ok(DbResult::Other(Value::None))
    }

    /// `detach`: close a session, which drops its handle and with it its
    /// namespace, database and variables.
    fn detach(&mut self, key: Option<Uuid>) -> Result<DbResult, TypesError> {
        let Some(id) = key else {
            return Err(invalid_params("Expected a session ID"));
        };
        self.attached
            .remove(&id)
            .map(|_| DbResult::Other(Value::None))
            .ok_or_else(|| session_not_found(id))
    }

    /// `sessions`: the ids of the sessions attached on this connection.
    fn sessions(&self) -> DbResult {
        let ids = self
            .attached
            .keys()
            .map(|id| Value::Uuid((*id).into()))
            .collect::<Vec<_>>();
        DbResult::Other(Value::Array(ids.into()))
    }
}

/// One session: its own namespace, database, variables and authentication.
struct Session {
    /// This session's own handle over the daemon's datastore.
    db: Surreal<Any>,
    /// Whether the client may use the data methods.
    authenticated: bool,
}

impl Session {
    fn new(shared: &Shared, authenticated: bool) -> Self {
        Self {
            db: shared.db.clone(),
            authenticated,
        }
    }

    async fn dispatch(
        &mut self,
        shared: &Shared,
        method: Method,
        params: Vec<Value>,
    ) -> Result<DbResult, TypesError> {
        if !self.authenticated && !allowed_before_auth(method) {
            return Err(TypesError::not_allowed(
                "authentication required: sign in with the MemCastle token as the password \
                 (see docs/database-access.md)"
                    .to_string(),
                NotAllowedError::Auth(AuthError::InvalidAuth),
            ));
        }
        match method {
            Method::Ping => Ok(DbResult::Other(Value::None)),
            Method::Version => Ok(DbResult::Other(Value::String(shared.version.clone()))),
            Method::Use => self.use_namespace(params).await,
            Method::Signin => self.signin(shared, params).await,
            Method::Authenticate => self.authenticate(shared, params).await,
            Method::Invalidate | Method::Reset => Ok(self.reset(shared)),
            Method::Set => self.set(params).await,
            Method::Unset => self.unset(params).await,
            Method::Info => self.info().await,
            Method::Query => self.query(params).await,
            Method::Live | Method::Kill => Err(lq_not_supported()),
            other => Err(unsupported(
                other,
                "only `query`, `use`, `let`, `unset`, `info`, `version`, `ping`, `signin`, \
                 `authenticate`, `invalidate`, `reset`, `attach`, `detach` and `sessions` are \
                 available; send a SurrealQL statement through `query` instead",
            )),
        }
    }

    /// `use`: switch this session's namespace and/or database.
    async fn use_namespace(&self, params: Vec<Value>) -> Result<DbResult, TypesError> {
        let mut params = params.into_iter();
        let (namespace, database) = (
            params.next().unwrap_or(Value::None),
            params.next().unwrap_or(Value::None),
        );
        // `none` and `null` both leave the selection alone: unselecting is not
        // something the embedded engine's client offers.
        if let Some(namespace) = optional_name(namespace)? {
            self.db.use_ns(namespace).await?;
        }
        if let Some(database) = optional_name(database)? {
            self.db.use_db(database).await?;
        }
        Ok(DbResult::Other(Value::None))
    }

    /// `signin`: accept the MemCastle token as the password.
    ///
    /// The username is ignored: there is one shared credential, as everywhere
    /// else in MemCastle (`docs/adr/014`). The reply is the token the client
    /// just presented, so that a later `authenticate` with it (which Studio
    /// does on reconnect) is checked the same way and a revoked token stops
    /// working at the next connection.
    async fn signin(
        &mut self,
        shared: &Shared,
        params: Vec<Value>,
    ) -> Result<DbResult, TypesError> {
        let Some(Value::Object(credentials)) = params.into_iter().next() else {
            return Err(invalid_params("Expected (params:object)"));
        };
        let password = ["pass", "password"]
            .iter()
            .find_map(|key| match credentials.get(*key) {
                Some(Value::String(password)) => Some(password.clone()),
                _ => None,
            });
        self.check_credential(shared, password.as_deref()).await?;
        Ok(DbResult::Other(Value::String(
            password.unwrap_or_else(|| "memcastle".to_string()),
        )))
    }

    /// `authenticate`: accept the token a previous `signin` handed back.
    async fn authenticate(
        &mut self,
        shared: &Shared,
        params: Vec<Value>,
    ) -> Result<DbResult, TypesError> {
        let Some(Value::String(token)) = params.into_iter().next() else {
            return Err(invalid_params("Expected (token:string)"));
        };
        self.check_credential(shared, Some(&token)).await?;
        Ok(DbResult::Other(Value::None))
    }

    /// Run `presented` through the daemon's authentication, recording the
    /// outcome on the session. Succeeds for anything when the daemon has
    /// authentication disabled, so Studio's login form works there too.
    async fn check_credential(
        &mut self,
        shared: &Shared,
        presented: Option<&str>,
    ) -> Result<(), TypesError> {
        let accepted = match &shared.auth {
            None => true,
            Some(authenticate) => authenticate(presented.map(str::to_string)).await,
        };
        self.authenticated = accepted;
        if accepted {
            return Ok(());
        }
        tokio::time::sleep(FAILED_SIGNIN_DELAY).await;
        // Fixed wording: nothing the client sent is echoed.
        Err(TypesError::not_allowed(
            "the token was not accepted".to_string(),
            NotAllowedError::Auth(AuthError::InvalidAuth),
        ))
    }

    /// `invalidate` and `reset`: forget this session's namespace, database,
    /// variables and authentication by starting from a fresh clone.
    fn reset(&mut self, shared: &Shared) -> DbResult {
        self.db = shared.db.clone();
        self.authenticated = shared.auth.is_none();
        DbResult::Other(Value::None)
    }

    /// `let`: set a session variable (or clear it when no value is given).
    async fn set(&self, params: Vec<Value>) -> Result<DbResult, TypesError> {
        let mut params = params.into_iter();
        let Some(Value::String(key)) = params.next() else {
            return Err(invalid_params("Expected (key:string, value:Value)"));
        };
        match params.next() {
            None | Some(Value::None) => self.db.unset(key).await?,
            Some(value) => {
                // `$auth`, `$session` and friends are the engine's own.
                surrealdb_rpc::check_protected_param(&key)?;
                self.db.set(key, value).await?;
            }
        }
        Ok(DbResult::Other(Value::Null))
    }

    /// `unset`: clear a session variable.
    async fn unset(&self, params: Vec<Value>) -> Result<DbResult, TypesError> {
        let Some(Value::String(key)) = params.into_iter().next() else {
            return Err(invalid_params("Expected (key)"));
        };
        self.db.unset(key).await?;
        Ok(DbResult::Other(Value::Null))
    }

    /// `info`: the record of the signed-in user, which for the embedded engine
    /// (no database users) is `NONE`.
    async fn info(&self) -> Result<DbResult, TypesError> {
        let mut response = self.db.query("SELECT * FROM $auth").await?;
        let rows: Value = response.take(0)?;
        let first = match rows {
            Value::Array(rows) => rows.into_vec().into_iter().next().unwrap_or(Value::None),
            other => other,
        };
        Ok(DbResult::Other(first))
    }

    /// `query`: run SurrealQL, with optional bound variables, and report one
    /// result per statement in SurrealDB's own shape.
    async fn query(&self, params: Vec<Value>) -> Result<DbResult, TypesError> {
        let mut params = params.into_iter();
        let Some(Value::String(sql)) = params.next() else {
            return Err(invalid_params("Expected (sql:string, vars?:object)"));
        };
        let variables = match params.next() {
            None | Some(Value::None | Value::Null) => None,
            Some(variables @ Value::Object(_)) => Some(variables),
            Some(_) => return Err(invalid_params("Expected (sql:string, vars?:object)")),
        };

        let mut query = self.db.query(sql);
        if let Some(variables) = variables {
            query = query.bind(variables);
        }
        let mut response = query.with_stats().await?;

        let statements = response.num_statements();
        let mut results = Vec::with_capacity(statements);
        for index in 0..statements {
            if let Some((stats, result)) = response.take::<Value>(index) {
                results.push(QueryResult {
                    time: stats.execution_time.unwrap_or_default(),
                    result,
                    query_type: QueryType::Other,
                });
            }
        }
        Ok(DbResult::Query(results))
    }
}

/// A `use` argument: a name, or `none`/`null` for "leave it".
fn optional_name(value: Value) -> Result<Option<String>, TypesError> {
    match value {
        Value::String(name) => Ok(Some(name)),
        Value::None | Value::Null => Ok(None),
        other => Err(invalid_params(format!(
            "Expected a string or none, got {other:?}"
        ))),
    }
}

/// The error for a method this endpoint does not implement, saying what to do
/// instead: a bare "method not allowed" would send someone to the docs for a
/// restriction that is really "not built yet".
fn unsupported(method: Method, hint: &str) -> TypesError {
    TypesError::not_allowed(
        format!(
            "`{}` is not supported by the MemCastle database admin endpoint: {hint}",
            method.to_str()
        ),
        NotAllowedError::Method {
            name: method.to_str().to_string(),
        },
    )
}
