use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use serde::de::{self, SeqAccess, Visitor};
use serde_json::{Value, json, value::RawValue};

const MAX_ARCHIVE: u64 = 256 * 1024 * 1024;
const MAX_JSON: u64 = 128 * 1024 * 1024;
pub(super) const MAX_CONVERSATION: usize = 16 * 1024 * 1024;

struct Conversations<'a> {
    wanted: Option<&'a str>,
}

impl<'de> Visitor<'de> for Conversations<'_> {
    type Value = Vec<Value>;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("an array of ChatGPT conversations")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut entries: A) -> Result<Self::Value, A::Error> {
        let mut collected = Vec::new();
        let mut index = 0;
        while let Some(raw) = entries.next_element::<Box<RawValue>>()? {
            if raw.get().len() > MAX_CONVERSATION {
                return Err(de::Error::custom(format!(
                    "conversation {index} exceeds 16 MiB"
                )));
            }
            let value: Value = serde_json::from_str(raw.get()).map_err(de::Error::custom)?;
            let id = super::validate_conversation(&value)
                .map_err(|error| de::Error::custom(format!("conversation {index}: {error}")))?;
            if self.wanted.is_none_or(|wanted| wanted == id) {
                collected.push(if self.wanted.is_some() {
                    value
                } else {
                    json!({"id": id})
                });
            }
            index += 1;
        }
        Ok(collected)
    }
}

fn parse(reader: impl Read, wanted: Option<&str>) -> Result<Vec<Value>, String> {
    let mut deserializer = serde_json::Deserializer::from_reader(BufReader::new(reader));
    let records =
        serde::de::Deserializer::deserialize_seq(&mut deserializer, Conversations { wanted })
            .map_err(|error| format!("invalid conversations.json: {error}"))?;
    deserializer
        .end()
        .map_err(|error| format!("invalid conversations.json: {error}"))?;
    Ok(records)
}

fn scan(path: &Path, wanted: Option<&str>) -> Result<Vec<Value>, String> {
    let mut file = std::fs::File::open(path).map_err(|_| {
        "cannot read the export file; check its path and the source's filesystem grant".to_string()
    })?;
    let size = file
        .metadata()
        .map_err(|_| "cannot inspect the export file".to_string())?
        .len();
    if size > MAX_ARCHIVE {
        return Err(
            "the export exceeds 256 MiB; split it into smaller conversations.json files".into(),
        );
    }
    let mut signature = [0; 4];
    let read = file
        .read(&mut signature)
        .map_err(|_| "cannot inspect the export format".to_string())?;
    if read == 4 && signature == *b"PK\x03\x04" {
        let mut archive = zip::ZipArchive::new(file)
            .map_err(|_| "the export ZIP is damaged or unsupported".to_string())?;
        // A path-qualified lookalike must not substitute for the expected top-level export file.
        let member = archive
            .by_name("conversations.json")
            .map_err(|_| "the export ZIP has no top-level conversations.json".to_string())?;
        if member.size() > MAX_JSON {
            return Err("conversations.json exceeds 128 MiB; split the export".into());
        }
        parse(member.take(MAX_JSON + 1), wanted)
    } else {
        if size > MAX_JSON {
            return Err("conversations.json exceeds 128 MiB; split the export".into());
        }
        file.seek(SeekFrom::Start(0))
            .map_err(|_| "cannot rewind conversations.json".to_string())?;
        parse(file.take(MAX_JSON + 1), wanted)
    }
}

pub(super) fn ids(path: &Path) -> Result<Vec<Value>, String> {
    scan(path, None)
}

pub(super) fn conversation(path: &Path, id: &str) -> Result<Option<Value>, String> {
    let mut matching = scan(path, Some(id))?;
    if matching.len() > 1 {
        return Err("the export has duplicate conversation IDs".into());
    }
    Ok(matching.pop())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn a_zip_archive_and_a_standalone_json_produce_the_same_conversations() {
        let json = include_bytes!("../fixtures/export/conversations.json");
        let dir =
            std::env::temp_dir().join(format!("memcastle-chatgpt-export-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let standalone = dir.join("conversations.json");
        let zipped = dir.join("archive.zip");
        std::fs::write(&standalone, json).unwrap();
        let file = std::fs::File::create(&zipped).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        writer
            .start_file(
                "conversations.json",
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
        writer.write_all(json).unwrap();
        writer.finish().unwrap();
        assert_eq!(ids(&zipped).unwrap(), ids(&standalone).unwrap());
        assert_eq!(
            conversation(&zipped, "conversation-a").unwrap(),
            conversation(&standalone, "conversation-a").unwrap()
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn malformed_records_fail_instead_of_advancing_a_cursor() {
        let dir =
            std::env::temp_dir().join(format!("memcastle-chatgpt-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("conversations.json");
        std::fs::write(
            &file,
            r#"[{"id":"valid","messages":[]},{"id":"missing-messages"}]"#,
        )
        .unwrap();
        assert!(ids(&file).unwrap_err().contains("conversation 1"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_archive_with_only_a_nested_conversations_file_is_refused() {
        let dir =
            std::env::temp_dir().join(format!("memcastle-chatgpt-nested-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("archive.zip");
        let mut writer = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        writer
            .start_file(
                "nested/conversations.json",
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
        writer.write_all(b"[]").unwrap();
        writer.finish().unwrap();
        assert!(
            ids(&path)
                .unwrap_err()
                .contains("top-level conversations.json")
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
