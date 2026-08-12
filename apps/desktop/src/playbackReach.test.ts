// @vitest-environment node
//
// The evidence behind the first row of the playback screen's "why nothing is
// playing" table (`clipPlayback.ts`, issue #52): **this window has no way to
// load a file from the disk.**
//
// That is a claim about three files in `src-tauri`, and a claim in a comment is
// true on the day it is written. This reads them. The day somebody enables the
// asset protocol, grants a file-system permission or widens the content
// security policy, one of these fails and brings them to the screen that says
// it cannot play anything — rather than leaving a paragraph quietly wrong in
// front of a user.
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
  it('has no asset protocol, so `convertFileSrc` has nothing to convert', () => {
    // Tauri serves a file from the disk through this and nothing else. It is
    // absent rather than disabled, which is the same thing to the runtime; the
    // assertion covers both so that turning it on either way is caught.
    const asset = security()['assetProtocol'] as Record<string, unknown> | undefined;

    expect(asset?.['enable'] ?? false).toBe(false);
  });

  it('grants no permission that reaches the file system', () => {
    // The whole of the window's privilege. Tauri denies what is not listed, so
    // this list is the answer to "what can the interface ask for" - and every
    // entry of it is accounted for here by name, so that a fourth permission
    // has to be looked at rather than added quietly.
    const capability = readConfig('capabilities/default.json');

    expect(capability['permissions']).toEqual([
      'core:window:allow-set-title',
      'core:event:allow-listen',
      'core:event:allow-unlisten',
    ]);
  });

  it('permits media from the bundle alone, through the policy’s own fallback', () => {
    const policy = directives(security()['csp'] as string);

    // No `media-src`, so a `<video>` is governed by `default-src`; and that is
    // `'self'`, which is the bundle Vite built. Neither `asset:` nor Tauri's
    // `http://asset.localhost` is reachable from either.
    expect(policy['media-src']).toBeUndefined();
    expect(policy['default-src']).toEqual(["'self'"]);
  });

  it('names no scheme anywhere in the policy that could carry a local file', () => {
    // Stated as a property rather than as a list of directives, so that a
    // scheme added to `img-src` for a thumbnail - which is the first thing
    // that would need one - is caught here too. A poster frame and a video stream reach
    // the window the same way.
    const policy = security()['csp'] as string;

    for (const scheme of ['asset:', 'asset.localhost', 'file:', 'stream:', 'clip:']) {
      expect(policy).not.toContain(scheme);
    }
  });
});
