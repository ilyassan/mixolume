import { AppWindow } from "lucide-react";
import { iconPngToDataUrl } from "@/lib/iconUrl";

interface SessionIconProps {
  iconPng: number[] | null;
  displayName: string;
}

export function SessionIcon({ iconPng, displayName }: SessionIconProps) {
  const dataUrl = iconPngToDataUrl(iconPng);

  if (!dataUrl) {
    return (
      <div className="bg-secondary text-muted-foreground flex size-8 shrink-0 items-center justify-center rounded-md">
        <AppWindow className="size-4" />
      </div>
    );
  }

  return (
    <img
      src={dataUrl}
      alt=""
      className="size-8 shrink-0 rounded-md object-contain"
      // Decorative - the app name is rendered as text right next to it.
      aria-hidden="true"
      title={displayName}
    />
  );
}
