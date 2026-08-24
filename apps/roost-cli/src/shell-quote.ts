// Quote one value for a POSIX shell so that every metacharacter stays
// literal. roost-cli has exactly one implementation because these strings
// are handed to bash both locally and over ssh (deploy journal programs,
// launchd bootstrap, remote lock commands): two diverging quoters would
// corrupt only the remote copy, which no hermetic test replays.
//
// BYTE-STABILITY: the body below must not change — callers embed its output
// into command strings compared byte-for-byte by recovery tooling.

/**
 * Wrap `value` in single quotes, escaping embedded single quotes with the
 * classic `'"'"'` close-escape-open idiom.
 */
export function posixShellQuote(value: string): string {
  return `'${value.replaceAll("'", `'\"'\"'`)}'`;
}
