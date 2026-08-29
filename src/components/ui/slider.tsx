import * as React from "react";
import * as SliderPrimitive from "@radix-ui/react-slider";

import { cn } from "@/lib/utils";

function Slider({
  className,
  defaultValue,
  value,
  min = 0,
  max = 100,
  ...props
}: React.ComponentProps<typeof SliderPrimitive.Root>) {
  const values = React.useMemo(
    () =>
      Array.isArray(value)
        ? value
        : Array.isArray(defaultValue)
          ? defaultValue
          : [min, max],
    [value, defaultValue, min, max],
  );

  // Boost-zone styling (VLC-style past-100% volume) -- only relevant to sliders whose `max` is
  // itself past 100 (the main volume slider on backends that support boosting; balance/L-R
  // sliders stay plain 0-100 and are unaffected). A flat on/off switch, not a blend toward the
  // boost color as the value climbs -- "how boosted" isn't the point, only "boosted or not" is,
  // so the whole bar goes fully to the boost color the instant it crosses 100%. `boostColor` is
  // computed once here and applied via inline `style` (not separate Tailwind classes per element)
  // to both the range fill and the thumb's border below, so they're driven by the literal same
  // value on the same render and can't fall out of sync with each other; the inline `transition`
  // is likewise explicit rather than relying on a `transition-colors` utility class, so there's no
  // ambiguity about whether it's actually applied.
  const isBoostable = max > 100;
  const currentValue = values[0] ?? min;
  const isBoosted = isBoostable && currentValue > 100;
  const boostColor = isBoosted ? "var(--color-boost)" : "var(--color-primary)";

  return (
    <SliderPrimitive.Root
      data-slot="slider"
      defaultValue={defaultValue}
      value={value}
      min={min}
      max={max}
      className={cn(
        "relative flex w-full touch-none items-center select-none data-[disabled]:opacity-50",
        className,
      )}
      {...props}
    >
      <SliderPrimitive.Track
        data-slot="slider-track"
        className="bg-secondary relative h-1.5 w-full grow overflow-hidden rounded-full"
      >
        {/* No CSS transition here -- Radix positions this element and the thumb below via two
            *independent* inline `left`/`right` styles. CSS-transitioning each separately let them
            visibly fall out of sync under rapid updates (confirmed live, including during an
            ordinary manual drag: the fill and the thumb briefly disagreed on the actual value).
            `value` is fed a single already-smoothed number from `useSmoothedNumber` (see
            `SessionRow.tsx`) instead, so both this and the thumb always read the exact same value
            on the exact same render -- they can't desync from something that's the same number. */}
        <SliderPrimitive.Range
          data-slot="slider-range"
          className={isBoostable ? "absolute h-full" : "bg-primary absolute h-full"}
          style={
            isBoostable
              ? { backgroundColor: boostColor, transition: "background-color 350ms ease-out" }
              : undefined
          }
        />
      </SliderPrimitive.Track>
      {values.map((_, index) => (
        <SliderPrimitive.Thumb
          data-slot="slider-thumb"
          key={index}
          className={cn(
            "bg-background block size-4 shrink-0 rounded-full border-2 shadow focus-visible:ring-4 focus-visible:ring-ring/50 disabled:pointer-events-none disabled:opacity-50",
            !isBoostable && "border-primary",
          )}
          style={
            isBoostable
              ? { borderColor: boostColor, transition: "border-color 350ms ease-out" }
              : undefined
          }
        />
      ))}
    </SliderPrimitive.Root>
  );
}

export { Slider };
