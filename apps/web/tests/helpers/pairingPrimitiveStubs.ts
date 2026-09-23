// Pairing DOM suites share one complete primitive mock because Bun module mocks
// are process-wide across test files. Optional capture hooks let a suite inspect
// dialog and button behavior without changing the rendered passthrough shape.

interface PairingPrimitiveCaptures {
  button?: (props: Record<string, unknown>) => void;
  dialog?: (props: Record<string, unknown>) => void;
}

let captures: PairingPrimitiveCaptures = {};

export function setPairingPrimitiveCaptures(next: PairingPrimitiveCaptures): void {
  captures = next;
}

function passthrough(props: Record<string, unknown>): unknown {
  return props.children;
}

export const pairingPrimitiveStubs = {
  Button: (props: Record<string, unknown>) => {
    captures.button?.(props);
    return props.children;
  },
  Card: (props: Record<string, unknown>) => [props.title, props.children],
  Chip: (props: Record<string, unknown>) => props.label,
  Dialog: (props: Record<string, unknown>) => {
    captures.dialog?.(props);
    return props.open
      ? [props.headline, props.description, props.children, props.actions]
      : null;
  },
  List: passthrough,
  ListRow: (props: Record<string, unknown>) => [props.headline, props.support, props.trailing],
  StatusDot: () => null,
  Surface: passthrough,
  TextField: () => null,
};
