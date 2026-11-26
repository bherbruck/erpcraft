import { de } from "date-fns/locale";
import { CalendarIcon } from "lucide-react";
import { type InputHTMLAttributes, useState } from "react";
import { Calendar } from "@/components/ui/calendar";
import { InputGroup, InputGroupAddon, InputGroupButton, InputGroupInput } from "@/components/ui/input-group";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { language } from "@/i18n/language";
import { t } from "@/i18n/t";
import { calendarDateToLocal, localToCalendarDate } from "@/lib/dates";
import { cn } from "@/lib/utils";

/**
 * An input for a calendar date: shadcn/ui's date picker (Popover and Calendar)
 * beside a field the date can be typed into, so a form can be filled without the
 * mouse. The value is the plain "YYYY-MM-DD" string the server uses; the
 * calendar shows it as that local day, so no time zone can move it.
 */
export function DateInput({
  value,
  onChange,
  className,
  disabled,
  ...props
}: Omit<InputHTMLAttributes<HTMLInputElement>, "value" | "onChange" | "type"> & {
  value: string;
  onChange: (value: string) => void;
}) {
  const [open, setOpen] = useState(false);
  const selected = calendarDateToLocal(value);
  return (
    <InputGroup className={cn("w-full", className)} data-disabled={disabled || undefined}>
      <InputGroupInput
        type="date"
        value={value}
        disabled={disabled}
        onChange={(event) => onChange(event.target.value)}
        // The browser's own picker gives way to the calendar beside the field; typing stays.
        className="[&::-webkit-calendar-picker-indicator]:hidden"
        {...props}
      />
      <InputGroupAddon align="inline-end">
        <Popover open={open} onOpenChange={setOpen}>
          <PopoverTrigger asChild>
            <InputGroupButton size="icon-xs" disabled={disabled} aria-label={t("date.pick")} title={t("date.pick")}>
              <CalendarIcon aria-hidden="true" />
            </InputGroupButton>
          </PopoverTrigger>
          <PopoverContent align="end" className="w-auto p-0">
            <Calendar
              mode="single"
              locale={language() === "de" ? de : undefined}
              captionLayout="dropdown"
              selected={selected}
              defaultMonth={selected}
              onSelect={(day) => {
                if (day) onChange(localToCalendarDate(day));
                setOpen(false);
              }}
            />
          </PopoverContent>
        </Popover>
      </InputGroupAddon>
    </InputGroup>
  );
}
