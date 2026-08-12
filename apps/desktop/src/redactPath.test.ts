import { describe, expect, it } from 'vitest';

import { redactPath, redactPathsIn } from './redactPath';

/**
 * The redaction the diagnostics report depends on (issue #101).
 *
 * Two properties, and they fail in opposite directions.
 *
 * The first is that **no directory component survives**. A report a user pastes
 * into a bug tracker goes further than a log file on their own disk, so a leak
 * here is worse than the leak `crates/logging/src/redact.rs` exists to prevent.
 *
 * The second is that this stays **the same function** as the Rust one. Two
 * implementations of a digest are two digests, and a report whose paths cannot
 * be lined up with the log lines about the same file is a report that has quietly
 * stopped being useful. The pinned case below is the exact string
 * `redact.rs::a_windows_drive_and_account_name_do_not_survive` pins, and
 * `docs/logging.md` prints, so an arithmetic difference between the two sides
 * fails here.
 */

/** The path both implementations pin, and what both must produce for it. */
const PINNED_PATH = String.raw`C:\Users\alice\Videos\Clipped\match.mkv`;
const PINNED_RENDERING = 'match.mkv#eb9715073a66288e';

describe('redacting a path', () => {
  it('produces the same string the Rust implementation pins for the same path', () => {
    expect(redactPath(PINNED_PATH)).toBe(PINNED_RENDERING);
  });

  it.each([
    ['the account name', 'alice'],
    ['the drive', 'C:'],
    ['a directory somebody chose', 'Videos'],
    ['a directory Clipped chose', 'Clipped'],
  ])('does not leave %s in the result', (_what, leaked) => {
    expect(redactPath(PINNED_PATH)).not.toContain(leaked);
  });

  it('keeps the file name, which is what makes the digest worth having', () => {
    expect(redactPath(PINNED_PATH).startsWith('match.mkv#')).toBe(true);
  });

  it('redacts a Windows path written with forward slashes, which Windows accepts', () => {
    const redacted = redactPath('C:/Users/alice/Videos/Clipped/match.mkv');

    expect(redacted.startsWith('match.mkv#')).toBe(true);
    expect(redacted).not.toContain('alice');
  });

  it('gives the same path the same digest, so a file can be followed', () => {
    expect(redactPath(PINNED_PATH)).toBe(redactPath(PINNED_PATH));
  });

  /*
   * Clipped names recordings after the game and the date, so two sessions of the
   * same game in two directories collide on the file name alone. The digest is
   * what keeps them apart, and a digest that ignored the directories would not.
   */
  it('keeps two files with the same name apart', () => {
    const first = redactPath('D:/clips/2026/match.mkv');
    const second = redactPath('D:/clips/2025/match.mkv');

    expect(first.startsWith('match.mkv#')).toBe(true);
    expect(second.startsWith('match.mkv#')).toBe(true);
    expect(first).not.toBe(second);
  });

  it('redacts a bare drive root to a digest with no name in front of it', () => {
    expect(redactPath('C:\\').startsWith('#')).toBe(true);
  });

  /*
   * The report is a block of lines, and a file name holding a newline would let
   * whatever came after it look like a field of its own. `#` is replaced for the
   * same reason: it is the separator, and a name carrying one would make the
   * rendering ambiguous.
   */
  it('cannot break the line it is written on', () => {
    const redacted = redactPath('D:/clips/two\nlines#1.mkv');

    expect(redacted).not.toContain('\n');
    expect(redacted.split('#')).toHaveLength(2);
  });

  it('caps a long file name rather than pasting it whole', () => {
    const redacted = redactPath(`D:/clips/${'a'.repeat(200)}`);

    expect(redacted.indexOf('#')).toBe(65);
    expect(redacted).toContain('~#');
  });

  /*
   * A file name outside Latin-1 is hashed as UTF-8 on both sides. Hashing UTF-16
   * code units here would agree with the Rust for every ASCII path and disagree
   * for this one, which is the sort of difference that is found years later.
   */
  it('hashes the bytes of a non-ASCII path, not its code units', () => {
    expect(redactPath('D:/clips/日本語.mkv')).not.toBe(redactPath('D:/clips/????.mkv'));
    expect(redactPath('D:/clips/日本語.mkv')).toContain('日本語.mkv#');
  });
});

describe('redacting the paths inside a sentence', () => {
  /*
   * The sentence `SupervisorError::ExecutableMissing` produces, which is the most
   * likely thing in a report from a machine where nothing works: an installation
   * with no recorder beside it (issue #226). It carries the account name in the
   * middle of a sentence somebody needs to read.
   */
  it('redacts the path in the sentence a missing recorder produces', () => {
    const said = redactPathsIn(
      'the recorder was not found at ' +
        String.raw`C:\Users\alice\AppData\Local\Programs\Clipped\clipped-recorder.exe` +
        ', so no recording can be started',
    );

    expect(said).not.toContain('alice');
    expect(said).toContain('clipped-recorder.exe#');
    expect(said).toContain('the recorder was not found at ');
    expect(said).toContain(', so no recording can be started');
  });

  /*
   * The full stop that ends the sentence is not part of the path. Digesting it
   * along with the path would give the same file two different digests depending
   * on where in a sentence it appeared, which is exactly the correlation the
   * digest exists for.
   */
  it('does not swallow the punctuation that ends the sentence', () => {
    const withStop = redactPathsIn(String.raw`written to C:\clips\match.mkv.`);

    expect(withStop.endsWith('.')).toBe(true);
    expect(withStop).toContain(redactPath(String.raw`C:\clips\match.mkv`));
  });

  it('redacts every path in a sentence, not only the first', () => {
    const said = redactPathsIn(
      String.raw`C:\Users\alice\a.mkv could not be moved to D:\Users\bob\b.mkv`,
    );

    expect(said).not.toContain('alice');
    expect(said).not.toContain('bob');
    expect(said).toContain('a.mkv#');
    expect(said).toContain('b.mkv#');
  });

  it('redacts a UNC share, where the leak is a machine name as well as a folder', () => {
    const said = redactPathsIn(String.raw`\\nas\alice-media\clips\match.mkv is unreachable`);

    expect(said).not.toContain('nas');
    expect(said).not.toContain('alice-media');
    expect(said).toContain('match.mkv#');
  });

  it('redacts the extended-length form as well as the ordinary one', () => {
    const said = redactPathsIn(String.raw`\\?\C:\Users\alice\Videos\match.mkv`);

    expect(said).not.toContain('alice');
    expect(said).toContain('match.mkv#');
  });

  /*
   * The one exemption, and it earns itself: the pipe name is in most of the
   * sentences the supervisor writes about a recorder it could not reach, it is
   * the thing somebody diagnosing that needs, and it names nothing of the user's.
   * A report that redacted it would be strictly less useful for no privacy gain.
   */
  it('leaves the recorder endpoint alone, which is a pipe name and not a location', () => {
    const said = String.raw`the recorder exited with status 1 without listening on \\.\pipe\clipped-recorder.1`;

    expect(redactPathsIn(said)).toBe(said);
  });

  it('leaves a sentence with no path in it exactly as it was', () => {
    const said = 'The recorder was asked to exit.';

    expect(redactPathsIn(said)).toBe(said);
  });

  /*
   * A relative name has no directory component, so there is nothing to redact and
   * a digest would only make the sentence harder to read. Asserted because the
   * tempting fix for a leak is to redact everything that looks like a file, and
   * that would make every report worse.
   */
  it('leaves a bare executable name readable', () => {
    const said = 'clipped-recorder.exe is not beside this application.';

    expect(redactPathsIn(said)).toBe(said);
  });
});
