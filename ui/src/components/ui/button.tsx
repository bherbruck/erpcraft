import { Slot } from "@radix-ui/react-slot";
import { LoaderCircle } from "lucide-react";
import type { ButtonHTMLAttributes, Ref } from "react";
import { cn } from "@/lib/cn";

type Variant = "default" | "outline" | "ghost" | "destructive" | "destructive-outline" | "link";
type Size = "default" | "sm" | "icon";

const variants: Record<Variant, string> = {
  default: "bg-primary text-primary-foreground hover:bg-primary/90",
  outline: "border border-input bg-background hover:bg-accent hover:text-accent-foreground",
  ghost: "hover:bg-accent hover:text-accent-foreground",
  destructive: "bg-destructive text-destructive-foreground hover:bg-destructive/90",
  "destructive-outline": "border border-destructive/50 bg-background text-destructive hover:bg-destructive-soft",
  link: "text-foreground underline underline-offset-4 hover:no-underline",
};

const sizes: Record<Size, string> = {
  default: "h-8 px-3",
  sm: "h-7 px-2 text-xs",
  icon: "size-8",
};

export interface ButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: Variant;
  size?: Size;
  /** Render the child element (a link, for example) with the button's look. */
  asChild?: boolean;
  /** Shows progress and disables the button while its command runs. */
  busy?: boolean;
  ref?: Ref<HTMLButtonElement>;
}

export function Button({
  className,
  variant = "default",
  size = "default",
  asChild = false,
  busy = false,
  disabled,
  children,
  type = "button",
  ...props
}: ButtonProps) {
  const classes = cn(
    "inline-flex shrink-0 items-center justify-center gap-1.5 whitespace-nowrap rounded-md text-sm font-medium transition-colors",
    "disabled:pointer-events-none disabled:opacity-50 [&_svg]:size-4 [&_svg]:shrink-0",
    variants[variant],
    sizes[size],
    className,
  );
  if (asChild) {
    return (
      <Slot className={classes} {...props}>
        {children}
      </Slot>
    );
  }
  return (
    <button type={type} className={classes} disabled={disabled || busy} aria-busy={busy || undefined} {...props}>
      {busy ? <LoaderCircle className="animate-spin" aria-hidden="true" /> : null}
      {children}
    </button>
  );
}
