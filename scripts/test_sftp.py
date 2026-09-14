"""Run SFTP integration tests against an isolated, loopback-only OpenSSH server."""
import getpass
import os
from pathlib import Path
import shutil
import socket
import subprocess
import tempfile
import time


def main():
    sshd = shutil.which("sshd") or "/usr/sbin/sshd"
    if not Path(sshd).is_file():
        raise SystemExit("This integration test requires a local OpenSSH server.")
    project = Path(__file__).resolve().parent.parent
    with tempfile.TemporaryDirectory(prefix="relay-sftp-test-") as folder:
        root = Path(folder)
        for name in ("host_key", "client_key"):
            subprocess.run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-f", str(root / name)], check=True)
        with socket.socket() as probe:
            probe.bind(("127.0.0.1", 0))
            port = probe.getsockname()[1]
        user = getpass.getuser()
        config = root / "sshd_config"
        config.write_text(f'''ListenAddress 127.0.0.1
Port {port}
HostKey {root / 'host_key'}
PidFile {root / 'sshd.pid'}
AuthorizedKeysFile {root / 'client_key.pub'}
StrictModes no
PasswordAuthentication no
KbdInteractiveAuthentication no
UsePAM no
AllowUsers {user}
Subsystem sftp internal-sftp
''')
        with (root / "server.log").open("w+") as log:
            server = subprocess.Popen([sshd, "-D", "-e", "-f", str(config)], stdout=log, stderr=log)
            try:
                for _ in range(100):
                    if server.poll() is not None:
                        log.seek(0)
                        raise RuntimeError(log.read())
                    try:
                        with socket.create_connection(("127.0.0.1", port), timeout=0.1):
                            break
                    except OSError:
                        time.sleep(0.05)
                else:
                    raise RuntimeError("Timed out waiting for the test SSH server")
                env = dict(os.environ, RELAY_TEST_KEY=str(root / "client_key"), RELAY_TEST_PORT=str(port), RELAY_TEST_USER=user)
                subprocess.run(["cargo", "test", "--locked", "real_sftp", "--", "--ignored", "--nocapture"], cwd=project, env=env, check=True)
            finally:
                server.terminate()
                try:
                    server.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    server.kill()
                    server.wait()


if __name__ == "__main__":
    main()
