// This agent-side module decides when to call MemCastle; it never reaches storage.
const plugin = {
  id: "assistant",
  // Add agent hooks here; call the daemon over MCP/HTTP for memory operations.
  server: async () => ({}),
  setup: async () => ({}),
}

export default plugin
