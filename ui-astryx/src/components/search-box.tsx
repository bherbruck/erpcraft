import { TextInput } from "@astryxdesign/core/TextInput";
import { Search } from "lucide-react";
import { useEffect, useState } from "react";

/** Search text for a list: shown as typed at once, handed on after a short pause. */
export function SearchBox({ value, onChange, label, busy }: { value: string; onChange: (next: string) => void; label: string; busy?: boolean }) {
  const [draft, setDraft] = useState(value);
  useEffect(() => setDraft(value), [value]);
  useEffect(() => {
    if (draft === value) return;
    const timer = setTimeout(() => onChange(draft), 250);
    return () => clearTimeout(timer);
  }, [draft, value, onChange]);
  return (
    <TextInput
      label={label}
      isLabelHidden
      type="search"
      placeholder={label}
      value={draft}
      onChange={setDraft}
      startIcon={Search}
      hasClear
      isLoading={busy}
      width={320}
      size="sm"
    />
  );
}
