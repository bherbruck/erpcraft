import { Search } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { Input } from "@/components/ui/input";

/**
 * A search field whose text lives in the URL. Typing updates the URL after a
 * short pause, so the server is not asked on every keystroke.
 */
export function SearchBox({
  value,
  onChange,
  label,
}: {
  /** The search text in the URL. */
  value: string;
  onChange: (value: string) => void;
  label: string;
}) {
  const [text, setText] = useState(value);
  const sent = useRef(value);

  // Back, or "clear search", changes the URL from outside.
  useEffect(() => {
    if (value !== sent.current) {
      sent.current = value;
      setText(value);
    }
  }, [value]);

  useEffect(() => {
    if (text === sent.current) return;
    const timer = setTimeout(() => {
      sent.current = text;
      onChange(text);
    }, 250);
    return () => clearTimeout(timer);
  }, [text, onChange]);

  return (
    <div className="relative w-64 max-w-full">
      <Search
        className="pointer-events-none absolute start-2.5 top-2 size-4 text-muted-foreground"
        aria-hidden="true"
      />
      <Input
        type="search"
        aria-label={label}
        placeholder={label}
        value={text}
        onChange={(event) => setText(event.target.value)}
        className="ps-8"
      />
    </div>
  );
}
