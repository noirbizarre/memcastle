"""__NAME__: a MemCastle source, written in Python and compiled to a WebAssembly component with componentize-py.

This one reads the `.txt` files of a flat directory, one document per file. `memcastle source build` runs
`componentize-py` (see `memcastle-source.toml`) to produce `dist/source.wasm`.

Runtime constraint: Python does not compile to WebAssembly directly. `componentize-py` embeds CPython in the component,
so the result is several megabytes and only the standard library is available unless you bundle packages with it
(pure-Python ones; anything with native code needs a WebAssembly build).
"""

import json
import os

from source import exports
from source.imports.types import Candidate, CanonicalDocument, Discovery, RawDocument, Segment, SourceKind, SourceRef
from source.imports.types import SourceError_CursorInvalid, SourceError_InvalidInput
from componentize_py_types import Err

NAME = "__NAME__"


def names(directory):
    return sorted(
        name for name in os.listdir(directory)
        if name.endswith(".txt") and os.path.isfile(os.path.join(directory, name))
    )


def parse_cursor(cursor):
    """The cursor is {"after": "<name>"}: everything up to and including that name is done. null is the beginning."""
    try:
        value = json.loads(cursor)
    except ValueError:
        raise Err(SourceError_CursorInvalid("the cursor is not JSON"))
    if value is None:
        return None
    after = value.get("after") if isinstance(value, dict) else None
    if not isinstance(after, str):
        raise Err(SourceError_CursorInvalid("`after` is missing or not a string"))
    return after


def revision_of(body):
    """A revision must change exactly when the content does. A real source would use the source's etag."""
    value = 0xCBF29CE484222325
    for byte in body.encode("utf-8"):
        value = ((value ^ byte) * 0x100000001B3) & 0xFFFFFFFFFFFFFFFF
    return "%016x-%d" % (value, len(body))


class Adapter(exports.Adapter):
    def identify(self, locator, options):
        # A source that accepts no options refuses one rather than ignoring it: a typo would otherwise mine everything.
        if options:
            raise Err(SourceError_InvalidInput("this source has no option `%s`" % options[0][0]))
        if locator is None:
            raise Err(SourceError_InvalidInput("give the directory to mine"))
        if not os.path.isdir(locator):
            raise Err(SourceError_InvalidInput("%s is not a directory this source can read" % locator))
        return SourceRef(source=NAME, account=None, locator=locator, options=[])

    def default_wing(self, source):
        return os.path.basename(source.locator.rstrip("/")) or "unnamed"

    def default_room(self):
        return "notes"

    def discover(self, source, cursor, limit):
        after = parse_cursor(cursor)
        rest = [name for name in names(source.locator) if after is None or name > after]
        candidates = [
            Candidate(external_id=name, cursor_after=json.dumps({"after": name}), handle=name)
            for name in rest[:limit]
        ]
        return Discovery(candidates=candidates, exhausted=len(rest) <= limit)

    def read(self, source, candidate):
        path = os.path.join(source.locator, candidate.handle)
        try:
            with open(path, encoding="utf-8") as handle:
                body = handle.read()
        except (OSError, UnicodeDecodeError):
            return None  # gone or unreadable since discovery: skipped, not an error
        return RawDocument(
            external_id=candidate.external_id,
            revision=revision_of(body),
            body=body,
            metadata=json.dumps({"path": path}),
            occurred_at=None,
        )

    def normalize(self, raw):
        metadata = json.loads(raw.metadata)
        return CanonicalDocument(
            title=raw.external_id,
            room=None,
            name=None,
            kind=SourceKind.FILE,
            uri=metadata.get("path"),
            tags=[],
            segments=[Segment(text=raw.body)],
        )
