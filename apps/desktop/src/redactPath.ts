/**
 * `RedactedPath`, in TypeScript.
 *
 * `crates/logging/src/redact.rs` reduces a path to its final component plus a
 * digest of the whole path, because a Windows user path begins with the account
 * name and a library path names the folders somebody chose. `docs/logging.md`
 * calls that the reason nothing in a log line carries a directory component.
 *
 * The diagnostics report is the other end of the same problem. It is composed in
 * the window, out of what the window was told, and what the window was told
 * includes full paths: the file a recording is writing, and any path the
 * supervisor put in a sentence — `the recorder was not found at
 * C:\Users\alice\AppData\Local\Programs\Clipped\clipped-recorder.exe`. A report a
 * user pastes into a bug tracker that leaked what the logs were careful not to
 * would undo the whole of that care (issue #101).
 *
 * # Why this is a second implementation and not the Rust one
 *
 * `tests/integration/tests/workspace_layering.rs` permits `apps/desktop/src-tauri`
 * exactly one crate of the repository's workspace, `clipped-ipc`, so the window
 * may not link `clipped-logging` to call the original — and a webview could not
 * call it directly in any case. What keeps the two from drifting is that the
 * rendering is pinned: `redactPath.test.ts` asserts the same string
 * `crates/logging/src/redact.rs` pins for the same input, so a change to either
 * side's arithmetic fails a test rather than producing two different digests for
 * one file.
 */

/** The longest file name kept, matching `FILE_NAME_LIMIT` in the Rust. */
const FILE_NAME_LIMIT = 64;

/** FNV-1a's 64-bit offset basis and prime. */
const OFFSET_BASIS = 0xcbf2_9ce4_8422_2325n;
const PRIME = 0x0000_0100_0000_01b3n;

/** 64 bits of ones, for the wrapping arithmetic `BigInt` does not do itself. */
const SIXTY_FOUR_BITS = 0xffff_ffff_ffff_ffffn;

/** UTF-8, which is what the Rust hashes: `to_string_lossy().as_bytes()`. */
const UTF8 = new TextEncoder();

/**
 * FNV-1a, 64-bit, over the UTF-8 bytes of a string.
 *
 * `BigInt` rather than a pair of 32-bit halves: the multiply is 64-bit and the
 * halves version of it is the kind of arithmetic that is wrong in one bit for a
 * year. This runs a handful of times when somebody opens a screen, so its cost
 * does not matter.
 */
function fnv1a64(value: string): bigint {
  let hash = OFFSET_BASIS;
  for (const byte of UTF8.encode(value)) {
    hash ^= BigInt(byte);
    hash = (hash * PRIME) & SIXTY_FOUR_BITS;
  }
  return hash;
}

/**
 * The final component of a path.
 *
 * Split on both separators, because Windows accepts both and this window only
 * ever sees Windows paths — the recorder it is talking to is a Windows process.
 * `Path::file_name` on the Rust side does the same thing when compiled for
 * Windows, which is the build that matters.
 */
function fileNameOf(path: string): string {
  const components = path.split(/[\\/]/);
  return components[components.length - 1] ?? '';
}

/**
 * Replaces anything that could break a line, and caps the length.
 *
 * `#` becomes `_` as well as the control characters, so that the separator in
 * the rendered form cannot appear inside the name it separates.
 */
function sanitise(fileName: string): string {
  const characters = [...fileName];
  const kept = characters
    .slice(0, FILE_NAME_LIMIT)
    .map((character) =>
      // eslint-disable-next-line no-control-regex -- the point is to match them.
      /[\u0000-\u001f\u007f]/.test(character) || character === '#' ? '_' : character,
    )
    .join('');

  return characters.length > FILE_NAME_LIMIT ? `${kept}~` : kept;
}

/**
 * A path reduced to something safe to send: its final component and a digest of
 * the whole path.
 *
 * ```ts
 * redactPath('C:\\Users\\alice\\Videos\\Clipped\\match.mkv');
 * // 'match.mkv#eb9715073a66288e'
 * ```
 *
 * Equal digests mean the same path, so a sequence of lines about one file can
 * still be followed and two files sharing a name stay apart. What it guarantees
 * is that no directory component survives — not the account name, not the drive
 * layout, not the folder names somebody chose. What it does **not** do is
 * anonymise the file name itself, which for a path the user chose can carry
 * meaning; it is a backstop rather than a licence.
 */
export function redactPath(path: string): string {
  const digest = fnv1a64(path).toString(16).padStart(16, '0');
  return `${sanitise(fileNameOf(path))}#${digest}`;
}

/**
 * Absolute paths, as they appear inside a sentence.
 *
 * Three shapes, because those are the three the recorder and the supervisor
 * actually produce: a drive-letter path with either separator, the extended
 * `\\?\` form, and a UNC share. A *relative* name — `clipped-recorder.exe` — is
 * deliberately not matched: it has no directory component, so there is nothing
 * to redact and replacing it with a digest would only make the sentence harder
 * to read.
 *
 * The trailing character class stops before whitespace and before the quoting
 * characters a message puts round a path, and `trimTrailingPunctuation` deals
 * with the full stop that ends the sentence.
 */
const ABSOLUTE_PATH = /(?:[A-Za-z]:[\\/]|\\\\[?][\\/]|\\\\[^\\/\s]+[\\/])[^\s"'<>|]*/g;

/**
 * The one absolute-looking thing that is left alone: a Windows named pipe.
 *
 * `\\.\pipe\clipped-recorder.1` is the endpoint the window and the recorder talk
 * over, and it is in half the sentences the supervisor writes about a recorder it
 * could not reach. It is a device-namespace name rather than a filesystem
 * location: it has no directories, names nothing of the user's, and is exactly
 * the detail somebody diagnosing "no recorder" needs. `\\.\` reaching anything
 * else — a raw volume, say — is not something Clipped produces, and would be
 * redacted like any other path because this only exempts the pipe namespace.
 */
const PIPE_NAMESPACE = /^\\\\\.[\\/]pipe[\\/]/i;

/** Sentence punctuation swept up by the path pattern's greedy tail. */
const TRAILING_PUNCTUATION = /[.,;:!?)\]}]+$/;

/**
 * Splits a match into the path and whatever punctuation ended the sentence.
 *
 * `at C:\...\clipped-recorder.exe, so no recording can be started` gives a match
 * ending in a comma; redacting the comma along with the path would digest a
 * string that is not the path, so two lines about the same file would no longer
 * agree.
 */
function trimTrailingPunctuation(matched: string): readonly [string, string] {
  const trailing = TRAILING_PUNCTUATION.exec(matched);
  if (!trailing) {
    return [matched, ''];
  }
  return [matched.slice(0, trailing.index), trailing[0]];
}

/**
 * Every absolute path in a piece of free text, redacted where it stands.
 *
 * The rule the diagnostics report keeps is that **no free text reaches the report
 * without going through this**. The recorder's own sentences are the ones that
 * carry paths — `SupervisorError::ExecutableMissing` and `::Spawn` both name one
 * — and they are also the sentences most worth sending, so scrubbing beats
 * dropping them.
 */
export function redactPathsIn(text: string): string {
  return text.replace(ABSOLUTE_PATH, (matched) => {
    if (PIPE_NAMESPACE.test(matched)) {
      return matched;
    }

    const [path, trailing] = trimTrailingPunctuation(matched);
    return `${redactPath(path)}${trailing}`;
  });
}
