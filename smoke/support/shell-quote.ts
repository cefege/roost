// Canonical POSIX shell quoting, the same rule as roost-platform's
// `posix_shell_quote`: specs build the exact command text the page inserts
// for an attachment and compare it byte for byte.

/**
 * Wrap `value` in single quotes, escaping embedded single quotes with the
 * classic `'"'"'` close-escape-open idiom.
 */
export function posixShellQuote(value: string): string {
  return `'${value.replaceAll("'", `'\"'\"'`)}'`;
}
