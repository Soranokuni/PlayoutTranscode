/**
 * Operator-facing wording for the `error_category` tokens on a failed job.
 *
 * The failed-job card printed the machine token (`db_mark_ready_failed`,
 * `io_disk_full`, `path_outside_watch_folder`…). PLAYOUT-CLIENT-DOCUMENTATION
 * §5.2 has already defined the wording for these, including the consequence
 * that matters most — that an encoded mezzanine is sitting in
 * `<target>\quarantine\` — and the web UI is where most operators look.
 *
 * Keep this table and §5.2 in sync: if a category is added on the server,
 * update both in the same commit.
 */
export interface ErrorCategoryInfo {
  /** Short chip text, replacing the raw token. */
  label: string
  /** One line under the error, saying what it means or what to do. */
  hint: string
  /**
   * The encode succeeded and only the bookkeeping failed, so a finished file
   * is in `<target>\quarantine\` that nothing references. An operator has to
   * know: the work was done.
   */
  quarantined?: boolean
}

export const ERROR_CATEGORIES: Record<string, ErrorCategoryInfo> = {
  source_missing_on_recovery: {
    label: 'Source file gone',
    hint: 'The job was pending across a restart and the source file is no longer there.',
  },
  fingerprint_failure: {
    label: 'Source unreadable',
    hint: 'The source file could not be read. It may still be copying, locked by another program, or on a share that went away.',
  },
  path_outside_watch_folder: {
    label: 'Not in the watch folder',
    hint: 'The input resolved outside the configured watch folder and was refused.',
  },
  db_insert_failed: {
    label: 'Could not register',
    hint: 'The registry entry could not be created, so nothing was transcoded. Check the service log.',
  },
  db_mark_ready_failed: {
    label: 'Encoded, not registered',
    hint: 'The transcode finished but the registry would not record it.',
    quarantined: true,
  },
  sidecar_write_failed: {
    label: 'Encoded, sidecar failed',
    hint: 'The transcode finished but the identity sidecar could not be written.',
    quarantined: true,
  },
  io_disk_full: {
    label: 'Not enough disk space',
    hint: 'Free space on the target volume, then retry.',
  },
  probe_failure: {
    label: 'Could not probe',
    hint: 'ffprobe could not read the stream layout. The file may be truncated or in an unsupported container.',
  },
  audio_measurement_failure: {
    label: 'Audio measurement failed',
    hint: 'The loudness pass did not complete, so the file could not be normalised to target.',
  },
  profile_disabled: {
    label: 'Profile disabled',
    hint: 'The profile this file maps to is turned off in the configuration.',
  },
  validation_failure: {
    label: 'Failed QC',
    hint: 'The encode finished but did not pass the output checks.',
  },
  transcode_failure: {
    label: 'Encode failed',
    hint: 'FFmpeg exited with an error. The stderr tail below usually says why.',
  },
  publish_failure: {
    label: 'Could not publish',
    hint: 'The encoded file could not be moved into the target folder.',
  },
  retryable_error: {
    label: 'Retrying',
    hint: 'A transient failure; the job will be attempted again.',
  },
  cancelled: {
    label: 'Cancelled',
    hint: 'An operator cancelled this job. Nothing was published.',
  },
  held_after_cancel: {
    label: 'Cancelled earlier',
    hint: 'You cancelled this file, so it was not ingested again at start-up. Ingest it now, or leave it; you will not be asked again for this file.',
  },
  // Legacy: the server has not produced this since Skipped became a phase
  // (T2-6), but old rows in the registry still carry it.
  duplicate_skipped: {
    label: 'Duplicate (legacy)',
    hint: 'An older version reported a skipped duplicate as a failure. It is not one.',
  },
}

/** Wording for a category, falling back to the raw token for anything the
 *  server has learned to emit since this table was written. */
export function describeErrorCategory(category?: string | null): ErrorCategoryInfo {
  if (!category) return { label: 'Failed', hint: '' }
  return ERROR_CATEGORIES[category] ?? { label: category, hint: '' }
}
