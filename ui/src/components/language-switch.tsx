import { Languages } from "lucide-react";
import { LANGUAGES, type Language, language, setLanguage } from "@/i18n/language";
import { t } from "@/i18n/t";
import { NativeSelect } from "./ui/native-select";

/** The language of the UI, chosen here and kept in this browser (per user when signed in). */
export function LanguageSwitch({ userId = null }: { userId?: string | null }) {
  return (
    <div className="flex items-center gap-1.5">
      <Languages aria-hidden="true" className="size-4 text-muted-foreground" />
      <NativeSelect
        aria-label={t("language.choose")}
        size="sm"
        value={language()}
        onChange={(event) => setLanguage(event.target.value === "de" ? "de" : ("en" satisfies Language), userId)}
      >
        {LANGUAGES.map((entry) => (
          <option key={entry.code} value={entry.code} lang={entry.code}>
            {entry.label}
          </option>
        ))}
      </NativeSelect>
    </div>
  );
}
