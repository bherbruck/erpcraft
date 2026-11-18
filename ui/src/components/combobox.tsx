import { ChevronsUpDown } from "lucide-react";
import { type KeyboardEvent, type ReactNode, type Ref, useEffect, useRef, useState } from "react";
import { Button } from "@/components/button";
import { Command, CommandInput, CommandList } from "@/components/ui/command";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { cn } from "@/lib/utils";

export function useDebounced<T>(value: T, delay: number): T {
  const [debounced, setDebounced] = useState(value);
  useEffect(() => {
    const timer = setTimeout(() => setDebounced(value), delay);
    return () => clearTimeout(timer);
  }, [value, delay]);
  return debounced;
}

/** A line of the list that is not a choice: loading, nothing found, an error, "20 of 4,000 shown". */
export function ComboboxNote({ children, tone = "muted" }: { children: ReactNode; tone?: "muted" | "error" }) {
  return (
    <p className={cn("px-2 py-1.5 text-sm", tone === "error" ? "text-destructive" : "text-muted-foreground")}>
      {children}
    </p>
  );
}

/**
 * The frame of every searchable combobox: shadcn/ui's combobox pattern, an
 * outline Button that opens a Popover holding a Command (cmdk) list.
 *
 * The list's content and the search belong to the caller, because the search
 * runs on the server (GUIDELINES.md, Lookups 2): cmdk's own filtering is off.
 * Typing on the closed field opens it with that letter already in the search,
 * so a line can be entered without the mouse; Enter takes the highlighted entry
 * and goes no further, so it does not also add a line in the grid underneath.
 */
export function Combobox({
  open,
  onOpenChange,
  query,
  onQueryChange,
  active,
  onActiveChange,
  searchable,
  searchLabel,
  searchPlaceholder,
  listLabel,
  shown,
  placeholder,
  trailing,
  children,
  id,
  "aria-label": ariaLabel,
  "aria-describedby": describedBy,
  invalid = false,
  required = false,
  disabled = false,
  triggerRef,
  className,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  query: string;
  onQueryChange: (query: string) => void;
  /** The `value` of the highlighted CommandItem. */
  active: string;
  onActiveChange: (value: string) => void;
  searchable: boolean;
  searchLabel: string;
  searchPlaceholder: string;
  listLabel: string;
  /** What the closed field shows; `null` shows the placeholder. */
  shown: ReactNode | null;
  placeholder: ReactNode;
  /** Beside the field, such as a link to the selected record. */
  trailing?: ReactNode;
  /** CommandGroup, CommandItem and ComboboxNote elements. */
  children: ReactNode;
  id?: string;
  "aria-label"?: string;
  "aria-describedby"?: string;
  invalid?: boolean;
  required?: boolean;
  disabled?: boolean;
  triggerRef?: Ref<HTMLButtonElement>;
  className?: string;
}) {
  const searchRef = useRef<HTMLInputElement>(null);
  const contentRef = useRef<HTMLDivElement>(null);

  function onTriggerKey(event: KeyboardEvent<HTMLButtonElement>): void {
    if (event.key.length === 1 && event.key !== " " && !event.ctrlKey && !event.metaKey && !event.altKey) {
      event.preventDefault();
      onQueryChange(searchable ? event.key : "");
      onOpenChange(true);
    } else if (event.key === "ArrowDown") {
      event.preventDefault();
      onOpenChange(true);
    }
  }

  return (
    <div className={cn("flex min-w-0 items-center gap-1", className)}>
      <Popover open={open} onOpenChange={onOpenChange}>
        <PopoverTrigger asChild>
          <Button
            ref={triggerRef}
            variant="outline"
            id={id}
            role="combobox"
            aria-expanded={open}
            aria-haspopup="listbox"
            aria-label={ariaLabel}
            aria-describedby={describedBy}
            aria-invalid={invalid || undefined}
            aria-required={required || undefined}
            disabled={disabled}
            onKeyDown={onTriggerKey}
            className="w-full min-w-0 flex-1 justify-between px-3 font-normal"
          >
            <span className={cn("truncate", shown === null && "text-muted-foreground")}>{shown ?? placeholder}</span>
            <ChevronsUpDown className="text-muted-foreground" aria-hidden="true" />
          </Button>
        </PopoverTrigger>
        <PopoverContent
          ref={contentRef}
          align="start"
          onCloseAutoFocus={(event) => {
            // The list closes with a short animation. If the user has moved on to another field
            // meanwhile, focus stays there instead of jumping back to this one.
            const focused = document.activeElement;
            if (focused && focused !== document.body && !contentRef.current?.contains(focused)) {
              event.preventDefault();
            }
          }}
          className="w-(--radix-popover-trigger-width) min-w-lookup max-w-[calc(100vw-2rem)] p-0"
          onOpenAutoFocus={(event) => {
            // Put the cursor after the letter that opened the list, instead of
            // selecting it, so the next keystroke continues the word.
            event.preventDefault();
            const input = searchRef.current;
            if (!input) return;
            input.focus();
            input.setSelectionRange(input.value.length, input.value.length);
          }}
        >
          <Command
            shouldFilter={false}
            loop
            value={active}
            onValueChange={onActiveChange}
            onKeyDown={(event) => {
              if (event.key === "Enter") event.stopPropagation();
            }}
          >
            <CommandInput
              ref={searchRef}
              value={query}
              onValueChange={onQueryChange}
              readOnly={!searchable}
              aria-label={searchLabel}
              placeholder={searchPlaceholder}
            />
            <CommandList label={listLabel}>{children}</CommandList>
          </Command>
        </PopoverContent>
      </Popover>
      {trailing}
    </div>
  );
}
