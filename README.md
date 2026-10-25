# cli2mcp

An MCP bridge converted from CLI for better permission control.

The program supports posix-compliant platform only.

## Usage

You should configure the server(via `config.toml`) and the client(harness-specific).

### `cli2mcp`(server) configuration

Refer to [config.example.toml](./config.example.toml)

```toml
[default]
timeout_ms = 500
# effect hint:
# * ro: read-only hint
# * write: default hint
# * destructive: destructive hint
effect = "ro"

[[tools]]
expose_name = "echo"
description = "Print a message"
exec = "printf"
args = ["%s\n", "Hello from cli2mcp"]
env_inherit = ["PATH"]
```

Merging behaviour: prioritise tools-specific config, 

### client configuration

For example using pi

```json
{
  "mcpServers": {
    "email_tools": {
      "command": "/usr/bin/cli2mcp",
      "args": [
        "/home/alice/.config/cli2mcp/himalaya.toml"
      ],
      "cwd": "/home/alice/.config/cli2mcp",
      "description": "Manipulate emails"
    }
  }
}
```

## Building

```bash
git clone https://github.com/js0ny/cli2mcp.git
cargo build --release
```

With `Nix`

```bash
nix build github:js0ny/cli2mcp
```

## Roadmap

- [ ] CLI Args: Expose ro only or do not expose destructive
- [ ] MCP Server: Connect via Streamable HTTP
- [ ] Config: `tools.success_codes`, a list of numbers that defines success code, prevent from reporting false negative.
    - For example, `rg` reports non zero when no matching 
- [ ] Config: `tools.output_stderr`, control whether a tool should output stderr in the return json ("never" | "on_error" | "always")
- [ ] MCP Server: Implement `resources/`, allow client reaches resources via uris

## License

SPDX: [MIT](https://spdx.org/licenses/MIT)
