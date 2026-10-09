import { readFile } from 'node:fs/promises';
import { extname } from 'node:path';
import { createHash } from 'node:crypto';

const mime = {
  '.xht': 'application/xhtml+xml', '.html': 'text/html', '.htm': 'text/html',
  '.css': 'text/css', '.js': 'application/javascript', '.png': 'image/png',
  '.svg': 'image/svg+xml', '.jpg': 'image/jpeg', '.gif': 'image/gif',
  '.ttf': 'font/ttf', '.woff': 'font/woff', '.woff2': 'font/woff2',
};
const sha256 = bytes => createHash('sha256').update(bytes).digest('hex');

export async function resourceResponse(path) {
  const body = await readFile(path);
  const headers = { 'content-type': mime[extname(path)] ?? 'application/octet-stream' };
  let sidecar;
  try {
    sidecar = await readFile(`${path}.headers`);
  } catch (error) {
    if (error.code !== 'ENOENT') throw error;
  }
  if (sidecar) {
    // Keep validation aligned with server.rs::sidecar_headers. In particular,
    // extensions are defaults, not authority over a WPT response's media type.
    for (const sourceLine of sidecar.toString('utf8').split(/\r?\n/)) {
      const line = sourceLine.trim();
      if (!line || line.startsWith('#')) continue;
      const colon = line.indexOf(':');
      if (colon < 0) continue;
      const name = line.slice(0, colon).trim();
      const value = line.slice(colon + 1).trim();
      if (!/^[A-Za-z0-9-]+$/.test(name) || /[\r\n]/.test(value)) continue;
      const key = name.toLowerCase();
      // The native server owns framing; sidecars cannot override these.
      if (key === 'content-length' || key === 'connection') continue;
      headers[key] = value;
    }
  }
  return { body, headers, protocol: {
    content_type: headers['content-type'], body_sha256: sha256(body),
    sidecar_sha256: sidecar === undefined ? null : sha256(sidecar),
  } };
}
