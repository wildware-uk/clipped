# 0014. A milestone is not a version, and the first release is v1.0.0

- Status: Accepted
- Date: 2026-08-17
- Issue: [#431](https://github.com/wildware-uk/clipped/issues/431)

## Context

[#431](https://github.com/wildware-uk/clipped/issues/431) asks for two things.
The first is a workflow that builds a release from a tag, and that is code:
`.github/workflows/release.yml` and `scripts/check-release-gates.ps1`. The
second is not code at all — *"an agent should be able to tag a milestone version
once its milestone is complete"*, which needs a rule rather than a judgement
made at the time, and names the ambiguity itself: *"`M9` complete does not
obviously mean `0.9.0`, and the answer should be stated rather than inferred."*

Three facts about this repository shape the answer.

**The milestone numbers are a plan, and the plan changes.** `M0` to `M15` group
scope in the order the work was planned (SPEC.md section 42). `M15 - Signal
Engine` was added on 2026-08-12, after `M0` to `M14` had been numbered, for
design that is not in SPEC.md at all. Nothing stops another being added
tomorrow, and nothing requires them to be finished in numeric order: measured
against the repository on 2026-08-17, `M0 - Project Foundations` has no open
issues while `M1 - Recording Engine` has ten and `M3 - Replay Buffer` has none.

**A published version number cannot be withdrawn.** It is the string a bug
report quotes, the string in Add or Remove Programs, and the string somebody
compares against the one they already have. Every other mistake in this
repository can be fixed by a commit; this one is on a stranger's machine.

**Every release costs the same, whatever it is called.** The installer bundles a
pinned LGPL v3 FFmpeg, so conveying it owes a notice, both licence texts, the
relinking permission and the *corresponding source of that exact build* —
mirrored somewhere that outlives BtbN's own release page, which deletes builds
after a few months (docs/licensing.md,
[#123](https://github.com/wildware-uk/clipped/issues/123)). The build is
unsigned, so every recipient gets a SmartScreen warning. None of that is reduced
by calling a release a preview.

Not in scope: what makes a change major, minor or patch *after* the first
release. That is ordinary semantic versioning over the compatibility surfaces —
configuration, the plugin API, recording metadata, the command line — and needs
no record of its own.

## Decision

**Milestone numbers never map to version numbers. The first release of Clipped
is `v1.0.0`, and it happens when every milestone is finished. Before that,
nothing is released at all.**

In practice:

- Finishing a milestone produces a closed milestone, not a version. There is no
  milestone tag, no preview, no alpha and no `0.x`.
- A milestone is finished when every issue in it is closed **and** a maintainer
  has closed the milestone on GitHub. Both are required, because neither implies
  the other: GitHub will close a milestone with fourteen open issues in it, and
  several issues here are open precisely because an acceptance criterion needs a
  human at a keyboard. The mechanical half a script can check; the other half is
  somebody accepting the claim.
- After `v1.0.0`, versions are ordinary semantic versioning over what changed.
  New milestones will keep being added; none of them will imply a version.
- Pre-releases are governed identically. `v1.0.0-rc.1` passes exactly the same
  gates, because a release candidate is something a stranger installs.
- The rule is enforced rather than trusted. `scripts/check-release-gates.ps1`
  refuses a tag while any milestone is open or has open issues, and
  `.github/workflows/release.yml` does not start the build unless it passes.
  The gate retires itself once anything has been published, so that a newly
  opened milestone cannot lock the project out of shipping a fix.
- An agent may push a tag and may open the version-bump pull request. An agent
  may not publish: the workflow always produces a draft, and a draft is not a
  distribution.

docs/releasing.md is the procedure that follows from this record.

## Alternatives

### The milestone number is the minor version — `M9` finished means `v0.9.0`

The reading the issue offers, and the attraction is real: it removes the
judgement entirely. The version becomes a lookup rather than a decision, which
is exactly what makes it safe to hand to an agent, and it gives anybody watching
the project a legible cadence — fifteen releases on the way to something, each
one meaning a named body of work now exists.

It loses on a fact about this repository rather than on taste: **the milestones
are not finished in numeric order.** `M0` is mechanically complete today and
`M1` has ten open issues; `M3` has none open and `M2` has eight. If `M3`
finishes before `M2`, the scheme demands `v0.3.0` before `v0.2.0`, which
semantic versioning forbids and which no package manager, updater or human will
read correctly. Waiting for them in order is not a fix — it makes the release
cadence hostage to whichever milestone is slowest, which is the opposite of the
responsiveness the scheme was bought for.

The second failure is worse because it is silent. Milestone numbers are
mutable: `M15` was inserted after the others were numbered, and another may be.
Under this scheme, inserting or renumbering a milestone renumbers versions that
have already been installed, which is not a thing that can be done.

### Ship `0.x` previews, decoupled from the milestone numbers

The strongest alternative, and it should not be dismissed lightly. Semantic
versioning's `0.x` explicitly disclaims stability, so it promises nothing that
would later have to be broken. More importantly, it addresses a genuine problem
with the decision taken here: a great many issues in this repository are open
because a criterion needs a human at a keyboard — a capture path exercised
against a real game, an installer run on a machine that has never built Clipped,
a recording checked by watching it. Shipping would recruit those humans. Not
shipping means every one of those criteria waits on the maintainer's own
machine, and a screen recorder is precisely the kind of software whose defects
live in other people's hardware.

It loses on cost rather than on principle. The expensive part of a release is
not the tag, it is the conveyance: the corresponding FFmpeg source mirrored
somewhere permanent, the relinking permission tested by substituting the DLLs
and confirming the application still records, the notices kept accurate as
dependencies move. That bill falls due on the first `0.1.0` and again on every
one after it. Paying it fifteen times to reach the same place is a real cost
against a benefit that a nightly build handed to a named tester delivers just as
well without conveying anything to the public.

**This is the alternative most likely to win later.** If the project reaches a
point where the remaining human-in-the-loop criteria are the critical path — and
that is a plausible way for this to end — the answer is a pull request against
this record and docs/releasing.md, argued on the evidence that the feedback is
worth the conveyance. It is not a flag on the workflow, and there is
deliberately no flag on the workflow, because the decision is worth a review
rather than a checkbox.

### Date-based versioning — `2026.8.0`

It sidesteps the question this record exists to answer: there is nothing to
decide, because the calendar decides. It is also honest about a project that
ships continuously rather than in compatibility epochs, and it removes the
argument about whether a given change is breaking.

It loses because Clipped has compatibility surfaces that a user is entitled to
be warned about, and a date cannot express a warning. The plugin API is a
promise to third-party authors (docs/plugin-api.md); the configuration file, the
recording metadata and the recorder's command line are all things somebody may
have built against. `2026.9.0` following `2026.8.0` says nothing about whether
their plugin still loads. A major version does.

### Write no rule, and decide at the time

Fewer words, and it has an honest case: the first release is a long way off, and
a rule written now is guesswork about circumstances nobody has seen.

It loses to the thing that prompted the issue. The request was that *an agent*
be able to tag a milestone version — and an agent, or a person in a hurry,
deciding a version number at the moment of tagging is the precise failure this
record exists to prevent. "Decide at the time" also has no artefact: there is
nothing for a reviewer to disagree with before the tag is pushed, and after it
is pushed the decision has already been made. A written rule can be argued with
in a pull request, which is the only point at which arguing is any use.

## Consequences

**Nobody can install Clipped until every milestone is finished.** This is the
whole cost, and it is large. The project gets no field evidence for the entire
build-out, on software whose hardest defects are in capture paths, encoder
drivers and audio devices that only exist on other people's machines. Anything
that would have been learned from users is instead learned later, or not at all.
Mitigating that without releasing — nightly builds handed to named testers,
under no conveyance obligation because they are not the public — is work nobody
has scoped.

**The rule maintains itself, including against the project.** The milestone gate
asks about every milestone that exists, so opening a new one for work nobody has
done re-locks the first release automatically, with no workflow change. `M15`
already did this on 2026-08-12. Anybody who wants to ship sooner has to either
finish the milestones or change this record in a reviewed pull request, which is
the correct amount of friction for a decision that cannot be reverted once
somebody has downloaded the result.

**The gate retires itself after the first release, deliberately.** Once anything
has been published, "every milestone finished" stops being asked, because a
milestone opened on Tuesday must not prevent a security fix on Wednesday. A
*draft* does not retire it — nobody has been given a draft. This is a real hole
in the rule's symmetry and it is accepted knowingly: after `v1.0.0`, what stops
a premature release is review, not a script.

**Raising the version is a chore, and that is the point.** The tree declares
`0.1.0` in 29 places across seven files — the workspace `Cargo.toml` and the
`version` requirement on each of its 21 `clipped-*` path dependencies,
`apps/desktop/src-tauri/Cargo.toml`, `tauri.conf.json` and four `package.json`
files — plus `Cargo.lock` and `package-lock.json`, which `--locked` and `npm ci`
will refuse if they lag. The release gate names every declaration that disagrees
with the tag rather than editing any of them, because a workflow that rewrote
the tree to match the tag would be choosing the version itself and the tag would
stop being evidence of anything.

**A release candidate costs exactly what a release costs.** No relaxed path
exists for `-rc.1`, so release candidates will be rare, and when the project
wants a cheap way to get a build in front of somebody it will have to build one
that is not a release. That is the right shape, but it is unbuilt.

**What to watch.** If the milestone set keeps growing faster than it closes, the
first release moves further away every month, and the `0.x` alternative above
gets stronger. The measurement is the count of open milestones over time; the
trigger for reopening this record is that the human-in-the-loop acceptance
criteria have become the critical path.
