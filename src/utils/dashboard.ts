export const CATEGORY_COLORS: Record<string, string> = {
  Documents: "#c4a574",
  Images: "#e08a3d",
  Videos: "#6b4f3a",
  Audio: "#8b6b4a",
  Archives: "#a67c52",
  Code: "#3d5a4c",
  Other: "#8c7f70",
};

export const AGE_BUCKETS = ["7d", "30d", "90d", "365d", "older"] as const;

export function categoryColor(name: string): string {
  return CATEGORY_COLORS[name] ?? CATEGORY_COLORS.Other;
}

export interface CategoryShare {
  category: string;
  files: number;
  bytes: number;
}

export function leadCategory(
  data: CategoryShare[],
  totalBytes: number,
): { category: string; percent: number } | null {
  if (!data.length || totalBytes <= 0) return null;
  const top = [...data].sort((a, b) => b.bytes - a.bytes)[0];
  if (!top || top.bytes <= 0) return null;
  return {
    category: top.category,
    percent: Math.round((top.bytes / totalBytes) * 100),
  };
}

export function percentOf(part: number, total: number): number {
  if (total <= 0 || part <= 0) return 0;
  return Math.max(0, Math.min(100, (part / total) * 100));
}
