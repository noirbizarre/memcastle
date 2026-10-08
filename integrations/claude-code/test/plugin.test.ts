import { expect, test } from "bun:test";

const root = new URL("../plugin/", import.meta.url);

test("the plugin declares native HTTP MCP, commands, hooks and user configuration", async () => {
  const manifest = await Bun.file(new URL(".claude-plugin/plugin.json", root)).json();
  const marketplace = await Bun.file(new URL(".claude-plugin/marketplace.json", root)).json();
  const mcp = await Bun.file(new URL(".mcp.json", root)).json();

  expect(manifest.name).toBe("memcastle");
  expect(manifest.mcpServers).toBe("./.mcp.json");
  expect(Object.keys(manifest.userConfig)).toEqual(["memcastle_mode", "memcastle_url", "memcastle_token"]);
  expect(marketplace.plugins[0].source).toBe(".");
  expect(mcp.mcpServers.memcastle.type).toBe("http");
  expect(mcp.mcpServers.memcastle.url).toBe("${user_config.memcastle_url}");
  expect(mcp.mcpServers.memcastle.headers.Authorization).toBe("Bearer ${user_config.memcastle_token}");
});
