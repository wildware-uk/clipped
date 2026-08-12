# Privacy and network access

Clipped is local-first. It records your games onto your disk and leaves them
there. There is no account, no service behind it and nothing that phones home.

This document is two things at once. The first half describes what Clipped does
and does not do today. The second half is the rule that keeps the first half
true as the project grows: Clipped will eventually need *some* network access —
checking for updates, or a highlight plugin reading a game's local Game State
Integration feed — and a policy that simply said "no network, ever" would be
quietly broken within a year. So the policy is not a ban. It is a requirement
that every instance is deliberate, opt-in, documented here, and visible in
review.

**Status.** Clipped is early in development. Detection, capture, the library and
the editor described below are being built, not shipped, so this document states
the policy the product is being built to and that every change is held to — not
a description of finished behaviour. Where a section covers something that does
not exist yet, it says so.

Related standards: SPEC.md section 39, AGENTS.md sections 13 and 14.

## Contents

- [The position](#the-position)
- [What stays on your machine](#what-stays-on-your-machine)
- [What Clipped does not collect](#what-clipped-does-not-collect)
- [What counts as network communication](#what-counts-as-network-communication)
- [Loopback and outbound are treated differently](#loopback-and-outbound-are-treated-differently)
- [The rule for introducing network communication](#the-rule-for-introducing-network-communication)
- [Register of network communication](#register-of-network-communication)
- [Plugin network access](#plugin-network-access)
- [Logs and diagnostics](#logs-and-diagnostics)
- [Changing this document](#changing-this-document)

## The position

```text
No telemetry
No account
No cloud upload
No automatic data transmission
```

Clipped works with no internet connection at all. Detection, capture,
encoding, the library, editing and export are entirely local operations, and a
machine that has never been online can do all of them. If a future feature
needs the network, that feature may be unavailable offline — recording must
not be.

No analytics, no behavioural tracking, no advertising SDKs, no crash uploads.
If crash reporting is ever added it is opt-in, and it appears in the
[register](#register-of-network-communication) below like anything else.

## What stays on your machine

Everything Clipped produces is an ordinary file or database row under a
directory you can open, back up or delete.

| Data | Where it lives | Leaves the machine |
| --- | --- | --- |
| Recordings, clips and screenshots | Normal media files in your recording directory | Only when you export or share them yourself |
| Library metadata: games, sessions, bookmarks, events, tags, favourites | A local SQLite database | Never |
| Settings, including per-game settings | Local configuration files | Never |
| Logs | Local log files | Only if you attach them to a bug report yourself |
| Detected game list and hardware capabilities | The local database | Never |
| Starting the recorder at sign-in, if you turn it on | One registry value named `Clipped Recorder` under `HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Run`, holding the path to `clipped-recorder.exe` | Never |

Everything above the last row is a file or a database row under a directory of
Clipped's own. **That last row is the one exception**, so it is stated plainly:
it is the only thing Clipped writes anywhere else on your computer, and it is
written only when you ask for it. `clipped-recorder start-at-login enable` puts
it there, `disable` removes it and leaves nothing behind, and `status` tells you
which it is. No installer writes it for you, nothing else in Clipped reads or
changes that key, and it is under `HKEY_CURRENT_USER`, so it applies to your
account and to nobody else signed in to the same machine. The value holds a
path and nothing about you. The decision is
[ADR 0006](adr/0006-recorder-lifetime-and-supervision.md).

Clipped does not delete or move your recordings on your behalf beyond the
retention rules you configure, and editing is non-destructive: the source
recording is never rewritten (AGENTS.md sections 56 and 57).

## What Clipped does not collect

Stated plainly, so that a future contributor can see what would be a
regression:

- No account, email address, username or licence key.
- No install identifier, machine identifier or hardware fingerprint sent
  anywhere. Hardware is inspected locally to choose an encoder; the result
  stays in the local database.
- No record of which games you play, how long you play them, or how often you
  use the application, sent anywhere.
- No recorded video, audio, screenshots or thumbnails uploaded, at any
  quality, for any purpose.
- No window titles, chat contents, microphone audio or file contents in
  anything transmitted. Separately, AGENTS.md section 13 keeps window contents,
  microphone content, private message contents and file contents out of the
  logs. Window titles are deliberately not on that list: a window title is how
  the recorder identifies a game, and SPEC.md section 36 requires game detection
  to be logged, so a title may well appear in a local log file. It is still
  never transmitted.
- No usage analytics, A/B testing, feature-flag service or remote
  configuration.

## What counts as network communication

For the purposes of this policy, network communication is **any operation that
causes bytes to leave the Clipped process over a network interface, or that
accepts bytes arriving over one**. Concretely, that includes:

- Opening a TCP or UDP socket, or a WebSocket, to any address.
- Listening on a socket, whatever it is bound to.
- Any HTTP or HTTPS request, including a HEAD request or a version check.
- DNS resolution, and local discovery protocols such as mDNS or SSDP.
- A dependency doing any of the above on Clipped's behalf. If a crate you add
  opens a socket, that is your change introducing network communication, not
  the crate's.
- Invoking another program that does any of the above — for example passing a
  URL to FFmpeg as an input, or shelling out to a downloader.

It does **not** include reading and writing local files, memory-mapped files,
Windows named pipes or shared memory used for on-machine inter-process
communication, or the recorder-to-UI IPC channel. Those never touch a network
stack. They are still subject to the security rules in AGENTS.md section 13,
but they are not what this policy governs.

## Loopback and outbound are treated differently

The two classes have genuinely different risk, so they get different rules.

**Loopback** means `127.0.0.1`, `::1` or `localhost`. Traffic is handled
entirely inside the kernel and never reaches a network adapter. A highlight
plugin receiving Counter-Strike 2 Game State Integration payloads is loopback:
the game posts to a port on the same machine, and nothing goes to the internet.

**Outbound** means anything else, *including the local network*. A request to
another machine on your LAN leaves your computer and is treated exactly like a
request to the internet. There is no "trusted LAN" category.

| | Loopback | Outbound |
| --- | --- | --- |
| Off by default | Not required — enabling the feature that needs it is the opt-in | Required |
| Documented in this file | Required | Required |
| Declared in the pull request | Required | Required |
| Binds or connects only to the loopback address | Required, never `0.0.0.0` | N/A |
| Authenticated | Required for listening sockets | Per protocol |
| Recording depends on it | Never | Never |

Two details matter for loopback in particular.

A **listening** socket bound to loopback is still reachable by every other
process on the machine, including a web page in a browser. Anything Clipped
listens on must therefore require a shared secret or token that the local
producer was configured with — for CS2 GSI, the auth token written into the GSI
configuration file — and must reject unauthenticated payloads rather than
trusting whatever arrives. It must bind to the loopback address explicitly;
binding to `0.0.0.0` exposes it to the LAN and is an outbound-class change
wearing a disguise.

Loopback is not a loophole for exfiltration. Sending data to a local process
that then forwards it to the internet is outbound communication, and is
governed as such.

## The rule for introducing network communication

Any change that introduces network communication under the definition above
must satisfy all of the following. This applies to first-party code, bundled
plugins and third-party plugins alike.

1. **Off by default.** Outbound communication must not happen in a default
   installation until the user turns it on. A loopback feature is exempt from
   this only when it is intrinsic to a feature the user has already explicitly
   enabled — enabling a CS2 highlight plugin *is* consent for that plugin's GSI
   listener.
2. **Explicit opt-in.** The user turns it on by a deliberate action: a setting,
   a prompt with a clear default of "no", or enabling a plugin whose network
   declaration they have seen. Consent for one purpose is not consent for
   another; a new purpose needs a new opt-in.
3. **Documented here.** Add a row to the
   [register](#register-of-network-communication) in the same pull request,
   stating what is sent, where to, when, what is *not* sent, and what happens
   when the feature is off or the network is unreachable.
4. **Minimal payload.** Send only what the stated purpose requires. Never
   recorded media, never an identifier that did not already need to exist,
   never anything AGENTS.md section 13 forbids logging.
5. **Fails closed and stays out of the way.** Unreachable network means the
   feature degrades quietly. Capture, encoding and muxing must never block on,
   retry against, or fail because of a network operation. If the machine is
   offline, recording still works.
6. **Declared in review.** The pull request template
   (`.github/pull_request_template.md`) has a network-access item. A pull
   request that introduces network communication must tick it and link both the
   documentation and the opt-in. Reviewers should treat an untouched network
   item on a change that adds a socket-opening dependency as a blocking
   problem.

A change that cannot satisfy points 1 to 5 should not be written. If you think
you have a genuine exception, raise it as an issue before writing the code, so
the decision is recorded rather than discovered in a diff.

## Register of network communication

**Clipped itself — the recorder and the desktop application — performs no
network communication of either class, and none of it is outbound.** Three
bundled *plugins* do, and they are the three rows below.

| Feature | Class | Destination | Default | Opt-in |
| --- | --- | --- | --- | --- |
| League of Legends highlight plugin | Loopback | `127.0.0.1:2999`, connect only | Off | Enabling the plugin, whose declaration is this row in the words `plugin.json` says it in |
| Counter-Strike 2 highlight plugin | Loopback, **listen** | Binds `127.0.0.1:3212`. Receives Game State Integration payloads from Counter-Strike 2 on the same machine. Sends nothing: it answers each POST with a status line and no body. | Off. The plugin does nothing until its configuration file is installed, and does not install one itself. | Running `clipped-cs2-plugin install <game folder>` by hand, which writes the one file that makes the game post at all. Enabling a plugin having read its declaration is the intended second step and there is no screen for it yet ([issue #281](https://github.com/wildware-uk/clipped/issues/281)), so today the install command is the whole of it. `clipped-cs2-plugin uninstall`, or deleting that file, ends it. |
| Dota 2 highlight plugin | Loopback, **listen** | Binds `127.0.0.1:3213`. Receives Game State Integration payloads from Dota 2 on the same machine. Sends nothing: it answers each POST with a status line and no body. | Off. The plugin is a separate executable, it does nothing unless something runs it, and nothing in the recorder starts a plugin yet. | Running the plugin, which writes the one configuration file that makes the game post at all — and says so, because Dota reads that directory at start-up. Enabling a plugin having read its declaration is the intended second step and there is no screen for it yet ([issue #281](https://github.com/wildware-uk/clipped/issues/281)), nor anything that records which plugins are enabled ([issue #282](https://github.com/wildware-uk/clipped/issues/282)), so today running it by hand is the whole of it. Deleting `gamestate_integration_clipped.cfg` from Dota's own configuration directory ends it. |

Each row is spelled out below, because a register entry that has to be decoded
is not a disclosure ([docs/plugin-api.md](plugin-api.md) is the design).

### What the League of Legends row means

- **What is sent:** an HTTP `GET` of `/liveclientdata/allgamedata`, with no
  body, no cookie and no authorisation header. Nothing about the machine, the
  user or the recording is in the request, and nothing is ever sent anywhere
  else.
- **When:** once a second, only while the plugin is attached to a running
  League of Legends process. Never when the game is not running, because the
  plugin is not running either.
- **What comes back:** the match's own state — the event list, the match clock
  and the players in it. It is read for kills, deaths, assists and the result,
  and what reaches the recording is the events, with the game's own fields
  attached. It is never sent anywhere.
- **When the network is unreachable or the API is not there:** nothing happens.
  The plugin keeps polling, says why in the log, tells the user after a minute
  of silence, and the recording is unaffected — capture never waits on it, and
  a plugin that fails is a plugin that stops, not a recording that stops.
- **The proxy question, since it decides whether "loopback" is true:** the
  request is made with `WINHTTP_ACCESS_TYPE_NO_PROXY`, so a system proxy
  configured on the machine cannot route it off the machine.
- **The redirect question, which decides the same thing:** the request is made
  with `WINHTTP_OPTION_REDIRECT_POLICY_NEVER`, so whatever is listening on port
  2999 cannot answer "go and ask this other server instead" and have the plugin
  do it. WinHTTP follows redirects by default; without this, and given that the
  certificate on this request is deliberately not validated
  ([docs/plugin-api.md](plugin-api.md)), any process that got to port 2999 first
  could have made this loopback row untrue. A test asserts the second listener
  is never reached.

### What the Counter-Strike 2 row means

What the plugin receives is a snapshot of your Counter-Strike match — the map,
the round, the score and your own kill, death and assist counts — and it stays
on the machine. What it becomes is worth being exact about, because it is less
than the finished feature: the plugin turns those snapshots into events on its
own standard output, and **nothing reads them yet**. A session can place an
event on a recording's timeline
([issue #71](https://github.com/wildware-uk/clipped/issues/71)), but nothing in
the recorder starts a plugin during a recording or feeds it what a plugin prints
([issue #338](https://github.com/wildware-uk/clipped/issues/338)); when that is
built, the destination is the local database, and this paragraph should say so
in the present tense at that point and not before. Either way nothing about it
is transmitted anywhere. The listener requires the
token from the configuration file on every payload and refuses anything else,
because a loopback port is reachable by every other process on this machine
(see [above](#loopback-and-outbound-are-treated-differently)), and it binds
`127.0.0.1` explicitly and never `0.0.0.0`.
[plugins/cs2/README.md](../plugins/cs2/README.md) documents the file it writes,
in full, and how to remove it. Recording does not depend on any of it: with the
plugin absent, disabled or unable to bind, Clipped records exactly as it
otherwise would.

### What the Dota 2 row means

- **What is sent: nothing.** The plugin only *receives*. It writes one
  configuration file into the game's own directory so that the game knows where
  to post, and it opens no outbound connection of any kind.
- **What is received:** the components the plugin subscribes to — the provider,
  the map, the player's own counters and their hero — from the game on this
  machine. Every payload has to carry a token the plugin generated and wrote
  into that configuration file; a payload without it is answered `403` and
  discarded, because a socket on `127.0.0.1` is reachable by every process on
  the machine, including a web page in a browser.
- **When the feature is off, or the socket cannot be bound:** the plugin reports
  the problem and exits. Recording is unaffected in either case, and it is
  unaffected while the plugin is running too — a recording never waits on a
  plugin (`docs/plugin-api.md`).

One thing is anticipated but **not implemented**, and has no code behind it
today. It is listed so that the shape of a future row is clear, not to imply it
exists:

- **Update checking.** If added, a plain request for a version file with no
  query parameters, no install identifier and no usage data attached, off
  unless the user enables it. It would gain a register row and a setting at
  that point.

## Plugin network access

**Status.** This section was written as **policy to be implemented in Milestone
9 (Highlight Plugin API)**. The contract exists now, and three plugins with it:
League of Legends ([issue #72](https://github.com/wildware-uk/clipped/issues/72)),
Counter-Strike 2
([issue #70](https://github.com/wildware-uk/clipped/issues/70)) and Dota 2
([issue #73](https://github.com/wildware-uk/clipped/issues/73)). Declaration and
consent below are implemented as types in `crates/plugins` and are covered by
tests. **Mediation is not**, and neither is the plugin manager that would show
you a declaration before you agreed to it
([issue #281](https://github.com/wildware-uk/clipped/issues/281)); nor does
anything in the recorder start a plugin during a recording yet
([issue #338](https://github.com/wildware-uk/clipped/issues/338)). The state of
each part is stated where it appears rather than left to be inferred
([docs/plugin-api.md](plugin-api.md) is how, and `crates/plugins` is where).

**Declaration** — *implemented*. A plugin declares its network access in its
manifest: the class (loopback or outbound), the destinations, whether it
listens or connects, and the purpose in one line. A plugin that declares
nothing is a plugin that is permitted nothing. A declaration that contradicts
itself — `loopback` naming an address that is not the loopback address, or an
`outbound` grant naming `127.0.0.1` — is refused rather than shown.

**Consent** — *implemented, and not yet shown to anybody*. The declaration is a
value the user's consent is recorded against, and a plugin whose declaration
has changed since it was allowed cannot be started until they are asked again —
"the consent lapses", enforced by a type. What does not exist yet is the screen
that shows it ([issue #281](https://github.com/wildware-uk/clipped/issues/281)),
so today the only way to read a bundled plugin's declaration is its
`plugin.json` and the row in the register above.

**Mediation** — **not implemented**. This paragraph described plugins reaching
the network through an interface the host provides, so that requests could be
checked against the declaration. That is not what M9 built: a plugin is a
separate process and opens its own sockets — the League plugin connects to
`127.0.0.1:2999` itself, and the Counter-Strike 2 and Dota 2 plugins bind
`127.0.0.1:3212` and `127.0.0.1:3213` themselves — so the declaration is
checked, rendered and consented to, and it is not a sandbox.
[Issue #280](https://github.com/wildware-uk/clipped/issues/280) is where an
AppContainer or job object makes it enforceable — possible *because* a plugin is
a process — and `NetworkAccess::ENFORCEMENT` is the sentence the user is shown
in the meantime, which does not overstate it. The rest of the paragraph holds
and is implemented: none of this may affect the recording, and a plugin that
hangs, floods or panics is stopped without capture noticing. That requirement
comes from SPEC.md section 2, which says that background analysis must never
interfere with the game, and from AGENTS.md sections 17 and 18.

**What enforcement can honestly promise.** How strong that mediation is depends
on the isolation model, and M9 chose **out of process**: a plugin is a directory
with an executable in it, which the recorder starts, talks to over a pipe and
can kill. That choice is what makes enforcement possible at all — an
in-process native plugin could call the operating system directly and could
never be held to a declaration — and it is the reason the promise above can
one day be kept. It is not kept today: nothing yet confines the child, and the
plugin manager must state the guarantee the user is actually getting rather
than implying the stronger one:

> Clipped shows what a plugin declares and refuses to start one whose
> declaration has changed since you allowed it. It cannot yet stop a plugin from
> using the network in ways it did not declare.

It lives in the code as `clipped_plugins::NetworkAccess::ENFORCEMENT`, so that
the day the guarantee changes, the wording the user reads changes with it. The
decision is argued in [plugin-api.md](plugin-api.md) and belongs in an ADR
([issue #279](https://github.com/wildware-uk/clipped/issues/279)).

**No exemptions.** Plugins shipped with Clipped declare their network access on
the same terms as third-party ones and appear in the register above.

## Logs and diagnostics

Logs are local files. Clipped never transmits them. They are useful in bug
reports, which means you attach them deliberately, having had the chance to
read them.

Because a game recorder sits next to sensitive content, AGENTS.md section 13
forbids logging window contents, microphone content, private message contents
and file contents. Diagnostics record what the recorder did — capture backend,
encoder, dropped frames, audio devices, plugin events — not what was on screen
or said.

The Diagnostics screen composes a **support report** — what the desktop
application can establish about the recorder — shows it to you in full, and
copies it to the clipboard when you ask. It transmits nothing: the clipboard is
yours, and what you do with it afterwards is your decision. It contains no
recorded media, no window title, no microphone audio and no file contents, and
every path in it is reduced to a file name and a digest of the whole path, so no
folder, drive or account name is in what you paste.
[diagnostics.md](diagnostics.md) lists every field it carries, and the list is
asserted by a test rather than only written down.

The support bundle proper — that report **and the log files**, written as one
archive — is not built. The window cannot reach the log files
([issue #303](https://github.com/wildware-uk/clipped/issues/303)). It must
contain no recorded media (SPEC.md section 36), and it will transmit nothing by
itself.

Forward reference: log content, levels and file locations are specified in
`docs/logging.md`, which is being written alongside this document
(issue #5). Where the two disagree about what may appear in a log,
`docs/logging.md` is the detail and AGENTS.md section 13 is the authority.

## Changing this document

If your change alters what Clipped stores, what it sends or what it collects,
update this file in the same pull request. A privacy policy that lags the code
by a release is worse than none, because people rely on it.

If you find that Clipped transmits something that is not in the register above,
that is a bug and worth an issue even if it looks harmless.
