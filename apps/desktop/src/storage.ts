import type { StorageLimits, StorageReport } from '@clipped/shared';
import { invoke } from '@tauri-apps/api/core';
import { useCallback, useEffect, useState } from 'react';

import { asProblem, type LibraryRead } from './library';

/**
 * What the library occupies, and what a storage limit would delete
 * (SPEC.md section 27, issue #95).
 *
 * # Every figure here is measured, and none of it is measured here
 *
 * The recorder walks the recording and trash folders, reads the volume and runs
 * the plan a sweep would carry out; this window asks it and draws the answer.
 * That is not a preference: this window may link neither `clipped-library` nor
 * `clipped-storage` (`tests/integration/tests/workspace_layering.rs`) and has no
 * file-system permission at all, so there is nothing here it could measure even
 * if it wanted to.
 *
 * So there is no fallback and no estimate. A report that could not be taken is
 * drawn as a report that could not be taken, because every figure this screen
 * could invent is one somebody would act on: "nothing would be deleted" is what
 * they would set a limit on the strength of, and "no free space" is what they
 * would delete recordings on the strength of (AGENTS.md sections 27 and 56).
 *
 * # The dry run
 *
 * {@link previewLimits} asks the same question against limits nobody has saved.
 * It is what makes a storage limit a control somebody can agree to rather than
 * one they discover the effect of afterwards: the recorder answers it with the
 * sweep's own measurement, so what the screen shows before a limit is saved
 * cannot disagree with what happens after it is (AGENTS.md section 55).
 *
 * **Nothing here writes anything.** The limits are saved through
 * `apply_settings` like every other setting (`settings.ts`), and the sweep is
 * what acts on them.
 */

/** Asks the recorder what the library occupies, against the configured limits. */
export async function readStorage(): Promise<StorageReport> {
  return invoke<StorageReport>('recorder_storage');
}

/**
 * Asks what a limit would delete, without saving it.
 *
 * The whole set of limits is sent, so a field left out is one the proposal does
 * not have — which is how "what would clearing this do" is asked at all.
 */
export async function previewLimits(limits: StorageLimits): Promise<StorageReport> {
  return invoke<StorageReport>('recorder_storage', { limits });
}

/** What the storage screen has, and how to ask again. */
export interface StorageView {
  /** The report, or why there is not one. */
  readonly read: LibraryRead<StorageReport>;
  /** Ask again — after a limit is saved, or after the trash is emptied. */
  readonly again: () => void;
}

/**
 * Reads the storage report when the screen opens, and again when asked.
 *
 * Not on a timer. A measurement is a directory walk at the other end, and a
 * screen that repeated it every few seconds would keep a disk busy for a figure
 * that changes when a recording ends. {@link StorageView.again} is what a save
 * calls.
 */
export function useStorage(): StorageView {
  const [read, setRead] = useState<LibraryRead<StorageReport>>({ state: 'reading' });
  const [asked, setAsked] = useState(0);

  useEffect(() => {
    let current = true;
    readStorage()
      .then((report) => {
        if (current) {
          setRead({ state: 'read', value: report });
        }
      })
      .catch((thrown: unknown) => {
        if (current) {
          setRead({ state: 'unread', problem: asProblem(thrown) });
        }
      });
    return () => {
      current = false;
    };
  }, [asked]);

  const again = useCallback(() => {
    setAsked((count) => count + 1);
  }, []);

  return { read, again };
}

/**
 * A number of bytes, in the unit that suits it.
 *
 * Powers of 1000, because that is what the settings file and the recorder's
 * bounds are in: `MINIMUM_QUOTA` is 1,000,000,000 and a drive sold as 1 TB holds
 * 1,000,000,000,000. A screen that showed a 250 GB quota as "232.8 GB" would be
 * disagreeing with the figure the person typed.
 */
export function size(bytes: number): string {
  const units = ['bytes', 'kB', 'MB', 'GB', 'TB'];
  let value = bytes;
  let unit = 0;
  while (value >= 1000 && unit < units.length - 1) {
    value /= 1000;
    unit += 1;
  }
  return unit === 0
    ? `${String(Math.round(value))} ${units[0] ?? 'bytes'}`
    : `${value.toFixed(value < 10 ? 1 : 0)} ${units[unit] ?? ''}`;
}

/**
 * The same figure a limit's field holds, read back in the unit a person reads.
 *
 * The field carries what the settings file carries — a number of bytes — because
 * that is what the recorder accepts and what its refusal names, and a window
 * with a second vocabulary for a setting is a window that can disagree with the
 * file (`clipped_ipc::settings`). This is a gloss under the field, not a second
 * value: it never travels.
 *
 * Absent for a value that is not a plain number of bytes, which includes the
 * word a limit that is not set is spelled with. Saying "0 bytes" there would be
 * a limit of nought, which is the one reading that must never be shown for the
 * absence of a limit.
 */
export function glossOf(value: string): string | undefined {
  const text = value.trim();
  if (!/^\d+$/.test(text)) {
    return undefined;
  }
  return size(Number(text));
}

/** The keys of the three limits, as the settings file spells them. */
export const MAXIMUM_USAGE = 'maximum_usage_bytes';

/** How much of the drive to leave free. */
export const MINIMUM_FREE_SPACE = 'minimum_free_space_bytes';

/** How old a recording may get. */
export const MAXIMUM_AGE_DAYS = 'maximum_age_days';

/** The three, in the order the screen draws them. */
export const LIMIT_KEYS = [MAXIMUM_USAGE, MINIMUM_FREE_SPACE, MAXIMUM_AGE_DAYS] as const;

/** What the recorder spells a limit that is not set as. */
export const NO_LIMIT = 'none';

/**
 * The limits a set of edited fields would save, as the wire spells them.
 *
 * `values` is what the fields hold, keyed as the settings file keys them; the
 * result is what {@link previewLimits} asks about. A field spelled
 * {@link NO_LIMIT}, or holding anything that is not a whole number, contributes
 * no limit — which is the right reading for the word and the only safe one for
 * a half-typed figure: a proposal is what the recorder measures against, and a
 * number nobody finished typing must not become a quota it plans deletions for.
 */
export function limitsFrom(values: Readonly<Record<string, string>>): StorageLimits {
  const figure = (key: string): number | undefined => {
    const text = values[key]?.trim() ?? '';
    return /^\d+$/.test(text) ? Number(text) : undefined;
  };

  const maximumUsage = figure(MAXIMUM_USAGE);
  const minimumFree = figure(MINIMUM_FREE_SPACE);
  const maximumAge = figure(MAXIMUM_AGE_DAYS);

  return {
    ...(maximumUsage === undefined ? {} : { maximum_usage_bytes: maximumUsage }),
    ...(minimumFree === undefined ? {} : { minimum_free_space_bytes: minimumFree }),
    ...(maximumAge === undefined ? {} : { maximum_age_days: maximumAge }),
  };
}
