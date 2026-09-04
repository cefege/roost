// Canonical POSIX shell quoting for deploy commands and browser attachment
// insertion. Callers import this through @roost/shared/shell-quote so local
// and remote command construction cannot diverge. The function body is
// byte-stable because recovery tooling compares generated commands exactly.

/**
 * Wrap `value` in single quotes, escaping embedded single quotes with the
 * classic `'"'"'` close-escape-open idiom.
 */
export function posixShellQuote(value: string): string {
  return `'${value.replaceAll("'", `'\"'\"'`)}'`;
}
