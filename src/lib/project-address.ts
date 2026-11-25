export interface AddressCandidate {
  url: string;
  port: number;
  host: string;
  priority: number;
}

const LOCAL_URL = /https?:\/\/(?:localhost|127\.0\.0\.1|0\.0\.0\.0|\[::1\]|\[::\]):\d{1,5}(?!\d)(?:[/?#][^\s<>"')\]]*)?/i;
const LOCAL_LABEL = /\blocal(?:\s+address)?\s*:\s*$/i;
const DEVTOOLS_LABEL = /\bdevtools\s*:/i;
const DEVTOOLS_PATH = /(?:^|\/)(?:__devtools__|__vue-devtools__)(?:\/|$)/i;
const PORT_HINT = /(?:\bport\b|端口)\D{0,8}?(\d{1,5})\b|listening on (\d{1,5})\b/i;
const ANSI = /\x1b\[[0-?]*[ -/]*[@-~]/g;

export function stripAnsi(line: string): string {
  return line.replace(ANSI, "");
}

export function portAddress(port: number, priority: number): AddressCandidate | null {
  if (!Number.isInteger(port) || port < 1 || port > 65535) return null;
  return { url: `http://localhost:${port}`, port, host: "localhost", priority };
}

export function isDevToolsPath(pathname: string): boolean {
  return DEVTOOLS_PATH.test(pathname);
}

export function parseDevelopmentAddress(line: string): AddressCandidate | null {
  if (DEVTOOLS_LABEL.test(line) || /in use|trying another|被占用|EADDRINUSE/i.test(line)) return null;
  const matched = LOCAL_URL.exec(line);
  if (matched) {
    try {
      const url = new URL(matched[0]);
      if (isDevToolsPath(url.pathname)) return null;
      const port = Number(url.port || (url.protocol === "https:" ? 443 : 80));
      if (!portAddress(port, 2)) return null;
      if (url.hostname === "0.0.0.0" || url.hostname === "[::]") url.hostname = "localhost";
      const priority = LOCAL_LABEL.test(line.slice(0, matched.index)) ? 3 : 2;
      return { url: url.href, host: url.hostname, port, priority };
    } catch {
      return null;
    }
  }
  const hint = line.match(PORT_HINT);
  return hint ? portAddress(Number(hint[1] ?? hint[2]), 1) : null;
}
