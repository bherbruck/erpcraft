import { useState } from "react";
import { readStored, writeStored } from "@/lib/storage";

/** State kept in the browser under a key; `null` as the key keeps it in memory only. */
export function useStoredState<T>(
  key: string | null,
  fallback: T,
  isValid: (value: unknown) => value is T,
): [T, (next: T) => void] {
  const [value, setValue] = useState<T>(() => (key ? (readStored(key, isValid) ?? fallback) : fallback));
  return [
    value,
    (next: T) => {
      setValue(next);
      if (key) writeStored(key, next);
    },
  ];
}
