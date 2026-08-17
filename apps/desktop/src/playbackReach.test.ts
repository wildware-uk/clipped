// @vitest-environment node
//
// What this window may load, now that it plays a recording (issue #304).
//
// It used to read these files to prove a **negative** — that nothing could
// reach the disk, which is why the playback screen drew no player. The player
// exists now, so the same three files have to be held to a different claim, and
// #304's last acceptance criterion is that claim: whatever privilege the window
// gained is the smallest thing that works, and this file describes what it now
// is rather than being deleted.
//
// The boundary, in one sentence: **the window still cannot name a file.** It has
// no asset protocol, no file-system permission and no `file:` anywhere in its
// policy. What it gained is one origin — `http://clip.localhost` — which is
// served by `src-tauri/src/playback.rs`, and that serves nothing at all until
// the recorder has answered `open_playback` for a recording. The interface is
// handed `http://clip.localhost/3`; a number nothing registered is a 404.
//
// The node environment for the same reason `stylesheet.test.ts` asks for it:
// the subject is configuration files as text, and there is no DOM in the
// question.

import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

import { describe, expect, it } from 'vitest';

/** A JSON file under `src-tauri`, read from disk rather than imported. */
function readConfig(relative: string): Record<string, unknown> {
  const path = fileURLToPath(new URL(`../src-tauri/${relative}`, import.meta.url));
  return JSON.parse(readFileSync(path, 'utf8')) as Record<string, unknown>;
}

/** A source file under `src-tauri`, as text. */
function readSource(relative: string): string {
  return readFileSync(fileURLToPath(new URL(`../src-tauri/${relative}`, import.meta.url)), 'utf8');
}

/** `app.security` out of `tauri.conf.json`. */
function security(): Record<string, unknown> {
  const app = readConfig('tauri.conf.json')['app'] as Record<string, unknown>;
  return (app['security'] ?? {}) as Record<string, unknown>;
}

/**
 * A content security policy as its directives.
 *
 * `default-src 'self'; img-src 'self' data:` becomes
 * `{ 'default-src': ["'self'"], 'img-src': ["'self'", 'data:'] }`.
 */
function directives(policy: string): Record<string, readonly string[]> {
  const parsed: Record<string, readonly string[]> = {};
  for (const directive of policy.split(';')) {
    const [name, ...sources] = directive.trim().split(/\s+/);
    if (name !== undefined && name !== '') {
      parsed[name] = sources;
    }
  }
  return parsed;
}

describe('what the window may load', () => {
  it('still has no asset protocol, so `convertFileSrc` has nothing to convert', () => {
    // The alternative that was rejected for playback, and the reason is in
    // `src-tauri/src/playback.rs`: the asset protocol serves any path inside a
    // scope, and a recording lives wherever the recorder's output directory
    // points — a setting — so the only scope that would work is every path on
    // the machine. It is absent rather than disabled, which is the same thing
    // to the runtime; the assertion covers both.
    const asset = security()['assetProtocol'] as Record<string, unknown> | undefined;

    expect(asset?.['enable'] ?? false).toBe(false);
  });

  it('still grants no permission that reaches the file system', () => {
    // The whole of the window's privilege, and **unchanged by the player**:
    // playing a recording added a URI scheme this process registers, not a
    // permission the interface holds. Tauri denies what is not listed, so this
    // list is the answer to "what can the interface ask for" — and every entry
    // is accounted for here by name, so that a fifth permission has to be
    // looked at rather than added quietly.
    //
    // `dialog:allow-save` is the one issue #399 added, and it is deliberately
    // not file-system reach: it lets the interface ask the operating system to
    // show a Save As dialog, and what comes back is a path a person typed. The
    // window still cannot read, write or list anything — the export it feeds is
    // performed by the recorder, which refuses a destination that already
    // exists (AGENTS.md section 56).
    //
    // `dialog:allow-open` is issue #51's, and it is the same shape: the folder
    // picker on the Settings screen, whose answer is a path a person chose and
    // which the window then sends to the recorder as a setting. It reaches no
    // file either - `tauri-plugin-fs` is present as a dependency of the dialog
    // plugin and is **not registered**, so none of its commands exists to be
    // permitted, and the directory itself is written to by the recorder.
    //
    // What is *not* here is the reason `open_recording`, `reveal_recording` and
    // `open_playback` are `#[tauri::command]`s rather than the interface
    // reaching a plugin or the file system itself: the permission that would
    // allow the first two is `opener:allow-open-path` over a scope, and a
    // recording lives wherever the recorder's output directory points, so the
    // only scope that would work is every path on the machine
    // (`src-tauri/src/main.rs`).
    const capability = readConfig('capabilities/default.json');

    expect(capability['permissions']).toEqual([
      'core:window:allow-set-title',
      'core:event:allow-listen',
      'core:event:allow-unlisten',
      'dialog:allow-save',
      'dialog:allow-open',
    ]);
  });

  it('permits media from the bundle and from one origin, and nothing else', () => {
    const policy = directives(security()['csp'] as string);

    // The whole of what the player gained. `'self'` is the bundle Vite built,
    // and `http://clip.localhost` is this application's own scheme — which
    // serves only what the recorder has already opened for playback.
    expect(policy['media-src']).toEqual(["'self'", 'http://clip.localhost']);
    // And everything else still falls back to the bundle: a widened
    // `default-src` would let the whole interface load from anywhere, which is
    // a much larger change than a `media-src` and would arrive looking like a
    // smaller one.
    expect(policy['default-src']).toEqual(["'self'"]);
  });

  it('names the origin its own scheme is served at, and no other', () => {
    // The drift this prevents: the scheme is registered in Rust and named in
    // JSON, and a policy naming an origin nothing serves would fail silently —
    // the element would be blocked and the screen would show a player that
    // never loads. So the two are read and compared rather than both being
    // trusted.
    const source = readSource('src/playback.rs');
    const origin = /const ORIGIN: &str = "([^"]+)"/.exec(source)?.[1];
    const scheme = /const SCHEME: &str = "([^"]+)"/.exec(source)?.[1];

    expect(scheme).toBe('clip');
    // Windows serves a custom scheme as `http://<scheme>.localhost`.
    expect(origin).toBe(`http://${String(scheme)}.localhost`);
    expect(directives(security()['csp'] as string)['media-src']).toContain(origin);
  });

  it('names no scheme anywhere in the policy that could carry a local file', () => {
    // Stated as a property rather than as a list of directives, so that a
    // scheme added to `img-src` for a thumbnail — which is the first thing that
    // would need one — is caught here too. A poster frame and a video stream
    // reach the window the same way, and the answer for both is that the file
    // is served by this application rather than reachable by the interface.
    const policy = security()['csp'] as string;

    for (const scheme of ['asset:', 'asset.localhost', 'file:', 'stream:', 'clip:']) {
      expect(policy).not.toContain(scheme);
    }
  });

  it('serves nothing the recorder has not opened for playback', () => {
    // The claim the origin above rests on, read from the file that keeps it: a
    // number reaches a path only through the registry, and the registry is
    // filled by `url_for`, which `main.rs` calls with what `open_playback`
    // answered and with nothing else. If the handler ever read a path out of
    // the request, this window would have the file-system reach the first two
    // cases say it has not got.
    const source = readSource('src/playback.rs');

    expect(source).toMatch(/fn respond\(/);
    // The address is parsed as a number. A handler that treated it as a file
    // name would not need this, and that is exactly the change this catches.
    expect(source).toMatch(/\.parse::<u64>\(\)/);
    // And `main.rs` registers the handler over the real registry rather than
    // over anything the interface supplies.
    const host = readSource('src/main.rs').replace(/\s+/g, ' ');
    expect(host).toMatch(/register_asynchronous_uri_scheme_protocol\( playback::SCHEME/);
    expect(host).toMatch(/playback::url_for\(std::path::Path::new\(&playback\.path\)\)/);
  });
});
