import * as React from "react";
import * as SliderPrimitive from "@radix-ui/react-slider";

import { cn } from "@/lib/utils";

function Slider({
  className,
  defaultValue,
  value,
  min = 0,
  max = 100,
  style,
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
        "relative flex h-5 w-full touch-none items-center select-none data-[disabled]:opacity-50",
        className,
      )}
      // `--radix-slider-thumb-transform` is Radix's own hook for this: the thumb's actual
      // positioning wrapper (internal to `@radix-ui/react-slider`, not something this file can
      // reach directly) sets `left` itself but leaves `transform: var(--radix-slider-thumb-
      // transform)` for the consuming app to fill in -- undefined, as it was here, that resolves
      // to `transform: none`, so the thumb was never actually centered on the track at all, only
      // positioned by its own default (browser/flex-dependent) static position. Confirmed live as
      // a real, reproducible bug, not just a visual nit: in WKWebView specifically, that static
      // position landed the thumb's clickable bounds mostly *above* the visible track, so only
      // roughly the top half of what looked like the slider ever actually registered a click --
      // the bottom half of the same visible thumb circle silently missed every time. `h-5` above
      // (taller than the thumb's own `size-4`) gives the root some breathing room either way, but
      // this is the actual centering fix; the height alone wouldn't have moved the thumb into it.
      style={{ "--radix-slider-thumb-transform": "translateY(-50%)", ...style } as React.CSSProperties}
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
        {/* `will-change: left, right`, not `transform` -- this element's actual animated
            properties are the `left`/`right` inline styles Radix sets directly (see the comment
            above), never `transform`. An earlier version of this hinted `transform`, which does
            nothing at all for an element that never animates that property -- so it never really
            tested whether promoting this to its own compositing layer helps with the "occasional
            flash of stale content on a frequently-updated element" class of symptom this exists
            to guard against; this is the first version that actually hints the right thing. */}
        <SliderPrimitive.Range
          data-slot="slider-range"
          className={isBoostable ? "absolute h-full" : "bg-primary absolute h-full"}
          style={{
            willChange: "left, right",
            ...(isBoostable
              ? { backgroundColor: boostColor, transition: "background-color 350ms ease-out" }
              : undefined),
          }}
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
          style={{
            willChange: "left, right",
            ...(isBoostable
              ? { borderColor: boostColor, transition: "border-color 350ms ease-out" }
              : undefined),
          }}
        />
      ))}
    </SliderPrimitive.Root>
  );
}

export { Slider };
