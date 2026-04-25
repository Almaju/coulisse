# MCP tool integration

Coulisse is a client for [Model Context Protocol](https://modelcontextprotocol.io) servers. Any MCP-compliant tool — a calculator, a filesystem browser, a REST API wrapper, your in-house data fetcher — becomes usable by any agent with a one-line config change.

## End-to-end example

Imagine a small MCP server that exposes a `say_hello` tool. Register it and hand it to an agent:

```yaml
providers:
  anthropic:
    api_key: sk-ant-...

mcp:
  hello:
    transport: stdio
    command: uvx
    args:
      - --from
      - git+https://github.com/macsymwang/hello-mcp-server.git
      - hello-mcp-server

agents:
  - name: greeter
    provider: anthropic
    model: claude-sonnet-4-5-20250929
    preamble: You greet people warmly.
    mcp_tools:
      - server: hello
```

Start the server. On boot you'll see Coulisse discover the server's tools and note them in the log.

Now the `greeter` agent can call `say_hello` whenever the model decides it's useful. Your client makes a normal chat completion request:

```bash
curl http://localhost:8421/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{
    "model": "greeter",
    "safety_identifier": "user-1",
    "messages": [
      {"role": "user", "content": "Please greet Alice."}
    ]
  }'
```

The model may call the tool one or more times; Coulisse runs the tool loop internally and returns only the final assistant message.

Under the hood, every invocation — tool name, arguments, result (or error) — is recorded against the assistant message that produced it, so you can replay the turn in the [studio UI](./studio-ui.md) and see which tools fired and what came back. This is tool-call capture for *debugging*, not an extension of the OpenAI surface: the wire response your SDK receives is unchanged.

## Transports

- **stdio** — good for local MCP servers you spawn yourself (Python scripts, Node programs, CLI tools). Coulisse manages the child process.
- **http** — good for long-running MCP services, especially ones shared across multiple Coulisse instances.

Both are configured the same way conceptually; see [MCP tools](../configuration/mcp.md) for fields.

## Scoping tools per agent

Different agents can see different subsets of tools, even from the same server:

```yaml
agents:
  - name: power-user
    mcp_tools:
      - server: filesystem      # every tool the filesystem server offers

  - name: read-only
    mcp_tools:
      - server: filesystem
        only:
          - read_file
          - list_files          # write / delete tools aren't exposed
```

This is Coulisse-side filtering — the model never sees the excluded tools, so it can't call them.

## Tool loop limits

Coulisse caps a single request at 8 tool-call turns. If the model hasn't produced a final answer by then, the request ends. This keeps runaway loops from billing you forever.

## Capture limitations

Tool-call capture only runs on the **streaming** path — every OpenAI SDK uses streaming for chat completions by default, so this covers normal usage. Non-streaming requests (`"stream": false`) still execute tools correctly; their invocations just aren't captured for the studio trail, because rig's non-streaming API doesn't expose intermediate events.

If a client disconnects mid-stream after a tool call has fired but before the result lands, the call is persisted with `result: null` so the studio UI still shows that the attempt happened.
