// A parser for server-sent events, because the dashboard cannot use `EventSource`: it cannot send the
// `Authorization` or `X-MemCastle-Mode` headers, and the token never goes in a URL (docs/adr/041). The stream is read
// with `fetch` instead and fed through this, a chunk at a time.

export interface SseFrame {
  /** The `event:` name; `message` when the frame names none, as the specification says. */
  event: string
  data: string
}

export class SseParser {
  private buffer = ""

  /** Feed the next piece of the stream; returns the frames it completed. A frame split across pieces waits for the rest. */
  push(chunk: string): SseFrame[] {
    // One line ending to handle: a frame is its lines up to a blank one, whichever of `\r\n`, `\n` or `\r` ended them.
    this.buffer += chunk.replace(/\r\n?/g, "\n")
    const frames: SseFrame[] = []
    for (let end = this.buffer.indexOf("\n\n"); end !== -1; end = this.buffer.indexOf("\n\n")) {
      const raw = this.buffer.slice(0, end)
      this.buffer = this.buffer.slice(end + 2)
      const frame = parseFrame(raw)
      if (frame) frames.push(frame)
    }
    return frames
  }
}

function parseFrame(raw: string): SseFrame | undefined {
  let event = ""
  const data: string[] = []
  for (const line of raw.split("\n")) {
    // A comment (the daemon's keep-alive) says the stream is alive and nothing else.
    if (line === "" || line.startsWith(":")) continue
    const colon = line.indexOf(":")
    const field = colon === -1 ? line : line.slice(0, colon)
    // A single space after the colon belongs to the syntax, not to the value.
    const value = colon === -1 ? "" : line.slice(colon + 1).replace(/^ /, "")
    if (field === "event") event = value
    else if (field === "data") data.push(value)
  }
  // A frame with no data is a comment or a field this client does not use: nothing to deliver.
  return data.length === 0 ? undefined : { event: event || "message", data: data.join("\n") }
}
