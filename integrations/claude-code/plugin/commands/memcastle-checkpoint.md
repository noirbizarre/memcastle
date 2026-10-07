---
description: Save a classified MemCastle checkpoint from this conversation
---

Read the installed `checkpoint-instructions` skill and classify only durable information from this conversation.
If MemCastle mode is `read-only` or `off`, explain that no checkpoint was submitted and do not call a write tool.
Otherwise call `memcastle_checkpoint` with the classified payload and show any daemon `help` text verbatim on failure.
