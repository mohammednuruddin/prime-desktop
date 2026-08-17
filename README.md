# Prime Desktop

Prime Desktop is a macOS Electron client for [Prime Agent](https://github.com/PrimeIntellect-ai/prime-agent). It provides a focused chat workspace with streaming responses, tool activity, subagent inspection, session resume, model selection, project tabs, permissions, and review surfaces.

## Requirements

- macOS
- Node.js 20 or newer
- A working `prime-agent` installation available on `PATH`

## Development

```bash
npm install
npm run dev
```

Useful checks:

```bash
npm run typecheck
npm test
npm run build
```

The app connects each opened project to Prime Agent’s resident daemon. Prime Agent remains the source of truth for sessions, queues, goals, schedules, resources, and harness state.

## Packaging

```bash
npm run package
```

The packaged application requires the same Prime Agent runtime and macOS permissions as the development app.

## Compatibility and security

- Supported runtime: Node.js 20, 22, or newer.
- The desktop client expects the installed Prime Agent daemon to provide the resident transport APIs used by `src/main/daemonTransport.ts`.
- IPC requests are accepted only from the current application window. Paths are checked before they reach the main process or daemon.
- The optional Prime Agent installer follows HTTPS redirects only on `app.primeintellect.ai`, limits the response size, checks that the response is a shell script, and supports `PRIME_AGENT_INSTALL_SHA256` for checksum pinning.
- Prime Agent workers run with the current macOS user permissions. They are not a security sandbox.
- Run `npm run check:release` before packaging. Packaging stops until the project owner adds a software license.

## Release checklist

1. Select and add a license file.
2. Pin or publish the Prime Agent version supported by the release.
3. Set `PRIME_AGENT_INSTALL_SHA256` when distributing the bundled installer flow.
4. Run `npm run typecheck`, `npm test`, and `npm run check:release`.

## License

No license has been selected yet. Add one before distributing this project for reuse.
