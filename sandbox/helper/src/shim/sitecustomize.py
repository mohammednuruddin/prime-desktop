# Startup shim for kernels the Desktop sandbox runs on Windows. Python imports this before anything else.
#
# 1. asyncio's Windows event loop builds a TCP socket pair on 127.0.0.1 to wake itself up, and the container has no
#    network, not even loopback to itself, so creating the loop blocks forever. This loop wakes itself by posting to its
#    completion port instead, which needs no socket.
# 2. The kernel's watchdog opens its parent process to notice when the host has gone. The parent is outside the container
#    and cannot be opened from inside it, so the kernel would exit at once. Its own process is one it can open.
import os
import sys

if os.name == "nt":
    try:
        os.environ["PRIME_AGENT_KERNEL_OWNER_PID"] = str(os.getpid())
        import _overlapped
        import asyncio
        from asyncio import windows_events

        _wake = object()  # the proactor keys finished work by an owner object; this one owns no socket

        class _NoFd:
            def fileno(self):
                return -1  # signal.set_wakeup_fd(-1) switches the wakeup off

            def close(self):
                pass

        class _Loop(windows_events.ProactorEventLoop):
            def _make_self_pipe(self):
                self._ssock = None
                self._csock = _NoFd()

            def _loop_self_reading(self, f=None):
                return

            def _close_self_pipe(self):
                self._csock = None

            def _write_to_self(self):
                # A completion with no overlapped makes the poll raise, so post a real one the proactor has registered:
                # it pops it, sets the future's result and moves on, and the poll has returned, which is the wake-up.
                proactor = self._proactor
                if proactor is not None:
                    try:
                        ov = _overlapped.Overlapped(0)
                        proactor._cache[ov.address] = (self.create_future(), ov, _wake, lambda *args: None)
                        _overlapped.PostQueuedCompletionStatus(proactor._iocp, 0, 0, ov.address)
                    except (OSError, AttributeError):
                        pass

        class _Policy(windows_events.WindowsProactorEventLoopPolicy):
            _loop_factory = _Loop

        asyncio.set_event_loop_policy(_Policy())
    except BaseException as error:  # a kernel that would hang is worse than one that stops and says why
        sys.stderr.write("desktop sandbox shim failed: %r\n" % (error,))
        os._exit(70)
