/** Search engines truncate meta descriptions past ~155 characters. */
const MAX_DESCRIPTION = 155;

/**
 * Clamp a meta description to `MAX_DESCRIPTION`, cutting on a word boundary.
 * Descriptions are authored short; this is a guardrail so a long frontmatter
 * string can never ship an over-length `<meta name="description">`.
 */
export function clampDescription(text: string): string {
  const trimmed = text.trim();
  if (trimmed.length <= MAX_DESCRIPTION) return trimmed;
  const cut = trimmed.slice(0, MAX_DESCRIPTION - 1);
  const lastSpace = cut.lastIndexOf(" ");
  return `${(lastSpace > 80 ? cut.slice(0, lastSpace) : cut).replace(/[,.;:\-\u2014]$/, "")}\u2026`;
}
