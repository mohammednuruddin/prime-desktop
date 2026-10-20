# Desktop sandbox

A helper that runs a command inside an OS sandbox. A host (the Electron app today, the Tauri app later) sends it a policy and a request as JSON and gets back what happened. It is a separate Cargo workspace on purpose: neither host owns it, and both call it the same way.

```
desktop-sandbox run --policy policy.json --request request.json [--backend auto|mxc|unsandboxed]
desktop-sandbox capabilities
desktop-sandbox attest [--backend mxc]
desktop-sandbox exec --policy policy.json [--write-cwd] [--write-env NAME] [--require-attested] -- <program> [args]
desktop-sandbox-probes --launcher desktop-sandbox --backend mxc     # the behavioural suite
desktop-sandbox-probes --self-check                                 # proves the suite can fail
```

## The contract

- `policy/` parses and validates a policy and a request **once**, into resolved types. Backends accept only those, so a relative path, a `..`, a path with an alternate data stream, an expired grant, an environment variable outside the allowlist or a limit out of range cannot reach a backend. Limits are rejected when out of range, never clamped.
- Protected names (`.git`, `.env`, `.ssh`, `.aws`, `.gnupg` by default) are denied at every depth under a root, in any case, and a protected name that is itself a link is denied through its target too.
- `open` network access is valid only with an expiring grant.
- `run` prints one `ExecutionResult`. Exit 0: it ran. 2: the policy or request was invalid. 3: the backend cannot hold this policy here and nothing was run.

## Running the agent's kernel

The agent's model runs its code only in a Python kernel that the agent starts as `<python> -m rlm.repl`, over plain pipes. `exec` runs a program with its standard input, output and error bridged live through the sandbox. Copied to a file named `desktop-kernel` and set as `PRIME_AGENT_KERNEL_PYTHON`, the helper becomes that interpreter: `PRIME_DESKTOP_REAL_PYTHON` names the real one and `PRIME_DESKTOP_KERNEL_POLICY` the policy. The agent's working directory and session directory become write roots, and the environment is cut down to the policy's allowlist, so the daemon's secrets and its internal control token never reach the model's code. It starts nothing unless the host passes attestation.

`kernel/check-kernel.mjs` runs the real engine against a scripted fake model, once with the kernel unprotected and once confined. On Linux (WSL2) the model's Python reads a private key, lists the home folder, reaches the network and sees a secret variable when unprotected, and none of those when confined, while it can still write its workspace and run a subprocess.

On Windows 11 the same check passes with the real engine (official 0.10.0 binary): the key, the home folder and the network are refused by the operating system, the secret variable is absent, and the workspace and a child process still work. Four things the Windows kernel needed, none of them in the engine:

- A virtual environment's `python.exe` is a launcher that cannot start in the container, so the host names the **base interpreter** (`PRIME_DESKTOP_REAL_PYTHON`) and the environment's packages (`PRIME_DESKTOP_KERNEL_PYTHONPATH`, which the policy must allow as `PYTHONPATH`).
- `ctypes`, which the agent's runtime and pandas import, cannot initialise without GUI access, so the kernel policy sets `allowGui`. The helper lists that among its limitations: it widens what the child can reach.
- asyncio's Windows loop wakes itself through a TCP socket pair on 127.0.0.1, and a container with no network blocks even that, so creating the loop hangs. A start-up shim (`helper/src/shim/sitecustomize.py`, written under the helper's state folder and mounted read-only) gives the loop a completion-port wake-up instead.
- The kernel's owner watchdog opens its parent process, which is outside the container and cannot be opened from inside it. The same shim points it at the kernel's own process.

Two things the kernel needed everywhere: the agent's own watchdog compares the daemon's process id with its parent and exits if the owner is gone, which is always true in a separate process namespace, so that variable is left out of the allowlist; and a venv whose interpreter link goes through a folder alias needs the alias in `readRoots`, which the policy now carries alongside the resolved path.

## What `enforcement` means

| Value | Meaning |
|---|---|
| `enforced` | The backend held the policy (as limited by `limitations`) **and** this host has passed the probe suite for this backend, this helper build and this OS build. |
| `unattested` | The backend applied the policy, but nothing proves it on this host yet. Run `attest`. Show it as unverified. |
| `unenforced` | Ran with no sandbox, by choosing the `unsandboxed` backend. Always shown to the user. |
| `unavailable` | The host cannot hold this policy. Nothing ran. |

There is no automatic fallback from a sandbox to running without one.

`limitations` lists what the backend cannot hold even when it enforces the rest. Read it before showing a state to a user.

## Backends

| Backend | State |
|---|---|
| `mxc` on Windows 11 (build 26100.9278 or later) | Works. A Microsoft process container through `mxc-sdk =1.0.0`, no admin. Probes: 32 pass, 0 fail, 1 known gap, 1 unavailable. |
| `mxc` on Linux | Works in WSL2 (bubblewrap 0.6.1): 28 pass, 0 fail, 2 known gaps, 1 unavailable. Not yet run on a native Linux machine. |
| `mxc` on macOS (15 or later) | Written (Seatbelt through the same SDK) and type-checked for both Mac CPUs, with lint clean. **Never run**: this setup has no Mac. The CI job on a macOS runner is the first real test. |
| `unsandboxed` | Works. The explicit full-access choice. |

Known limits (also in `docs/memory/mxc-windows-limits.md`):

- All systems: a protected name the command creates itself is not denied, because the backends deny by path.
- Linux and macOS: the number of processes is not limited (Windows enforces it with a job object). The suite reports it as a known gap.
- Linux: a denied path is hidden behind an empty mount, so a write into it succeeds into the mount and vanishes. The real file is untouched, which is what the suite checks.
- Windows: native git, Node's `realpath.native` and Git Bash do not work inside the container; Node and PowerShell need `allowGui`.
- Only `none` and `open` networking can be held. Address and proxy rules are not wired.

## Testing

The probe suite (`probes/`) runs a small program under the helper and checks each allow and deny case against what really happened. A deny case passes only when the program reports an attempted and refused operation, so a program that never ran cannot pass. The suite also runs against an unsandboxed control, and a test requires every OS-held deny case to fail there: that shows the probes can see an open door.

Cargo on this project's Windows PC: Smart App Control sometimes blocks a freshly built unsigned executable (os error 4551), and a blocked probe program makes every case report unavailable. The development wrapper signs the debug binaries with the local research certificate before running them, which avoids it; a rebuild with a different `-C metadata` is the fallback.

Programs that import USER32 (Node, PowerShell) fail inside the container when the helper is started from a WSL shell, and run when it is started from a process in the user's own session.
