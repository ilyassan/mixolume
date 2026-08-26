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
          className="bg-primary absolute h-full"
        />
      </SliderPrimitive.Track>
      {values.map((_, index) => (
        <SliderPrimitive.Thumb
          data-slot="slider-thumb"
          key={index}
          className="border-primary bg-background block size-4 shrink-0 rounded-full border-2 shadow transition-colors focus-visible:ring-4 focus-visible:ring-ring/50 disabled:pointer-events-none disabled:opacity-50"
        />
      ))}
    </SliderPrimitive.Root>
  );
}

export { Slider };
