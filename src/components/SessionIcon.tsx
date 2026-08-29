import { AppWindow } from "lucide-react";
import { useIconObjectUrl } from "@/hooks/useIconObjectUrl";

interface SessionIconProps {
  iconPng: number[] | null;
  displayName: string;
}

export function SessionIcon({ iconPng, displayName }: SessionIconProps) {
  const objectUrl = useIconObjectUrl(iconPng);

  if (!objectUrl) {
    return (
      <div className="bg-secondary text-muted-foreground flex size-8 shrink-0 items-center justify-center rounded-md">
        <AppWindow className="size-4" />
      </div>
    );
  }

  return (
    <img
      src={objectUrl}
      alt=""
      className="size-8 shrink-0 rounded-md object-contain"
      // Decorative - the app name is rendered as text right next to it.
      aria-hidden="true"
      title={displayName}
    />
  );
}
