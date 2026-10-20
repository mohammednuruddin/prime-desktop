import json, os, socket, subprocess, sys
out = {}
def t(name, fn):
    try: out[name] = fn()
    except Exception as e: out[name] = f"ERR {type(e).__name__}: {e}"
# An absolute path from the host: `~` resolves from variables the sandbox removes, which would make a block look earned.
home = os.environ.get("CHECK_HOME") or os.path.expanduser("~")
t("cwd", os.getcwd)
t("write_cwd", lambda: open("py_wrote.txt", "w").write("x"))
t("read_private_key", lambda: open(os.path.join(home, ".ssh", "id_test")).read()[:20])
t("list_home", lambda: sorted(os.listdir(home))[:6])
t("subprocess", lambda: subprocess.run([sys.executable, "-c", "print('from-child')"], capture_output=True, text=True).stdout.strip())
t("network", lambda: bool(socket.create_connection(("1.1.1.1", 443), timeout=3)) and "CONNECTED")
t("env_has_secret", lambda: "PRIME_DESKTOP_SECRET_ENV" in os.environ)
print(json.dumps(out))
