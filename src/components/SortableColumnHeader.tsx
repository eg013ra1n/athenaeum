import { ChevronUp, ChevronDown, ChevronsUpDown } from "lucide-react";

interface SortableColumnHeaderProps {
  field: string;
  label: string;
  currentSortField: string | null;
  sortDirection: "asc" | "desc";
  onSort: (field: string) => void;
  align?: "left" | "right" | "center";
}

export default function SortableColumnHeader({
  field,
  label,
  currentSortField,
  sortDirection,
  onSort,
  align = "left",
}: SortableColumnHeaderProps) {
  const isActive = currentSortField === field;

  const SortIcon = () => {
    if (!isActive) {
      return <ChevronsUpDown size={14} className="text-content-muted" />;
    }
    return sortDirection === "asc" ? (
      <ChevronUp size={14} className="text-accent" />
    ) : (
      <ChevronDown size={14} className="text-accent" />
    );
  };

  const alignClass = align === "right" ? "justify-end" : align === "center" ? "justify-center" : "justify-start";

  return (
    <button
      onClick={() => onSort(field)}
      className={`flex items-center gap-1 hover:text-content transition-colors w-full ${alignClass}`}
    >
      {label}
      <SortIcon />
    </button>
  );
}
