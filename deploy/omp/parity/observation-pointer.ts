export function pointerValue(value: unknown, pointer = ""): unknown {
  if (pointer === "") return value;
  if (!pointer.startsWith("/")) throw new Error(`Invalid observation JSON pointer: ${pointer}`);
  for (const token of pointer.slice(1).split("/")) {
    const key = token.replaceAll("~1", "/").replaceAll("~0", "~");
    if (Array.isArray(value)) {
      if (!/^(?:0|[1-9]\d*)$/.test(key)) return undefined;
      value = value[Number(key)];
    } else if (value !== null && typeof value === "object") value = Object.getOwnPropertyDescriptor(value, key)?.value;
    else return undefined;
  }
  return value;
}
