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
npm run build
```

The app connects each opened project to Prime Agent’s resident daemon. Prime Agent remains the source of truth for sessions, queues, goals, schedules, resources, and harness state.

## Packaging

```bash
npm run package
```

The packaged application requires the same Prime Agent runtime and macOS permissions as the development app.

## License

No license has been selected yet. Add one before distributing this project for reuse.
