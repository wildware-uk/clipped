import { railPanelId, railTabId, SectionRail, type RailSection } from '@clipped/ui';
import { useState, type ReactNode } from 'react';

import { HotkeyList } from './HotkeyList';
import {
  HOTKEYS_SECTION,
  NOTHING_IS_EDITABLE,
  SETTINGS_SECTIONS,
  type SettingRow,
  type SettingsSection,
} from './settings';

/**
 * The Settings screen (issue #51).
 *
 * The deck draws a rail of sections and a pane of controls. The rail is here and
 * the controls are not, because **this window cannot read or write a single
 * setting**: `settings.ts` sets out why, and it is a fact about what is built
 * rather than about what was finished. What each pane carries instead is how the
 * setting is set today and the work that has to land before this window can hold
 * it — so somebody who came here to change something leaves able to change it
 * (AGENTS.md sections 27 and 45).
 *
 * The rail is real: it is the one thing on this screen that does something, and
 * what it does is move between sections.
 */

/** The name the rail's element ids are built from. */
const RAIL = 'settings';

/** The rail's entries, which are the sections themselves. */
const RAIL_SECTIONS: readonly RailSection[] = SETTINGS_SECTIONS.map((section) => ({
  id: section.id,
  label: section.label,
}));

/** One setting's row in a section's table. */
function Row({ row }: { readonly row: SettingRow }): ReactNode {
  return (
    <tr>
      <th scope="row">
        {row.label}
        {row.key ? (
          <>
            {' '}
            <code className="clipped-code">{row.key.name}</code>
          </>
        ) : null}
      </th>
      <td>
        {row.today}
        {row.run ? (
          <>
            {' '}
            <code className="clipped-code">{row.run}</code>
          </>
        ) : null}
      </td>
      <td className="clipped-muted">{row.needs}</td>
    </tr>
  );
}

/** The pane one section of the rail opens. */
function Pane({ section }: { readonly section: SettingsSection }): ReactNode {
  return (
    <div
      className="clipped-screen__pane"
      id={railPanelId(RAIL, section.id)}
      role="tabpanel"
      aria-labelledby={railTabId(RAIL, section.id)}
      /*
       * A tab stop of its own, which is what WAI-ARIA asks of a tab panel
       * holding no focusable element: without it, tabbing off the rail leaves
       * the window entirely and the pane the rail just opened is reachable by
       * pointer alone.
       *
       * Suppressed rather than dropped because the rule itself agrees — its own
       * default options are `roles: ['tabpanel']`, and `jsx-a11y`'s strict
       * preset restates the rule with no options at all, which is what takes
       * that allowance away. AGENTS.md section 42: a local suppression, with
       * the reason.
       */
      // eslint-disable-next-line jsx-a11y/no-noninteractive-tabindex
      tabIndex={0}
    >
      <h2 className="clipped-screen__heading">{section.label}</h2>
      <p className="clipped-screen__lead clipped-muted">{section.lead}</p>

      {/*
       * The one section with live state behind it. A hotkey the user cannot have
       * is a key that does nothing and says nothing, and the recorder is the only
       * process that knows — so unlike every other setting on this screen, this
       * one has an answer worth asking for (issue #232).
       */}
      {section.id === HOTKEYS_SECTION ? <HotkeyList /> : null}

      <table className="clipped-table">
        <thead>
          <tr>
            <th scope="col">Setting</th>
            <th scope="col">How it is set today</th>
            <th scope="col">What this window needs first</th>
          </tr>
        </thead>
        <tbody>
          {section.rows.map((row) => (
            <Row key={row.label} row={row} />
          ))}
        </tbody>
      </table>
    </div>
  );
}

/** The Settings screen. */
export function SettingsScreen(): ReactNode {
  const [openId, setOpenId] = useState(SETTINGS_SECTIONS[0]?.id ?? '');
  const open = SETTINGS_SECTIONS.find((section) => section.id === openId) ?? SETTINGS_SECTIONS[0];

  return (
    <>
      <h1 className="clipped-screen__title">Settings</h1>

      <p className="clipped-screen__lead">
        Clipped’s settings are global, with per-game overrides on top of them. The recorder holds
        them, and applies them to a recording when a game launches.
      </p>

      {/*
       * The one statement the rest of the screen is read against, in the marked
       * panel the unbuilt screens and the Games screen's detection state both
       * use — this is the one paragraph here that has to be read. It is not a
       * live region: unlike the recorder's state, nothing about it changes while
       * somebody is looking at it.
       */}
      <section className="clipped-panel" aria-label="Why nothing here can be changed">
        <h2 className="clipped-panel__heading">{NOTHING_IS_EDITABLE.heading}</h2>
        <p className="clipped-panel__body">{NOTHING_IS_EDITABLE.why}</p>
        <p className="clipped-panel__body clipped-muted">{NOTHING_IS_EDITABLE.instead}</p>
      </section>

      <div className="clipped-screen__split">
        <SectionRail
          label="Settings sections"
          name={RAIL}
          sections={RAIL_SECTIONS}
          currentId={open?.id ?? ''}
          onSelect={(section) => {
            setOpenId(section.id);
          }}
        />
        {open ? <Pane section={open} /> : null}
      </div>
    </>
  );
}
