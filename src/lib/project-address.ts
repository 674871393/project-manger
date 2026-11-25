export interface AddressCandidate {
  url: string;
  port: number;
  host: string;
  priority: number;
}

const LOCAL_URL = /https?:\/\/(?:localhost|127\.0\.0\.1|0\.0\.0\.0|\[::1\]|\[::\]):\d{1,5}(?!\d)(?:[/?#][^\s<>"')\]]*)?/i;
const PORT_HINT = /(?:\bport\b|端口)\D{0,8}?(\d{1,5})\b|listening on (\d{1,5})\b/i;
const ANSI = /\x1b\[[0-?]*[ -/]*[@-~]/g;

export function stripAnsi(line: string): string {
  return line.replace(ANSI, "");
}

export function portAddress(port: number, priority: number): AddressCandidate | null {
  if (!Number.isInteger(port) || port < 1 || port > 65535) return null;
  return { url: `http://localhost:${port}`, port, host: "localhost", priority };
}

export function parseDevelopmentAddress(line: string): AddressCandidate | null {
  if (/in use|trying another|被占用|EADDRINUSE/i.test(line)) return null;
  const matched = line.match(LOCAL_URL)?.[0];
  if (matched) {
    try {
      const url = new URL(matched);
      const port = Number(url.port || (url.protocol === "https:" ? 443 : 80));
      if (!portAddress(port, 2)) return null;
      if (url.hostname === "0.0.0.0" || url.hostname === "[::]") url.hostname = "localhost";
      return { url: url.href, host: url.hostname, port, priority: 2 };
    } catch {
      return null;
    }
  }
  const hint = line.match(PORT_HINT);
  return hint ? portAddress(Number(hint[1] ?? hint[2]), 1) : null;
}
