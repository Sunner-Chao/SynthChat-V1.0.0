import { BookOpen, Camera, Newspaper } from "lucide-react";
import { useAppStore } from "../lib/store";
import type { AppSection } from "../lib/types";
import { MenuRow } from "../components/common";

export function DiscoverPanel() {
  const { moments, worldbooks, setSection } = useAppStore();
  const entries: Array<{ id: AppSection; title: string; meta: string; icon: typeof Newspaper }> = [
    { id: "moments", title: "朋友圈", meta: `${moments.length} 条动态`, icon: Camera },
    { id: "worldbooks", title: "世界书", meta: `${worldbooks.length} 本世界书`, icon: BookOpen },
  ];
  return (
    <section className="primary-panel embedded-panel">
      <div className="panel-title action-title">
        <div className="panel-title-text"><span>Discover</span><strong>发现</strong></div>
      </div>
      <div className="menu-card" style={{ margin: "0 16px" }}>
        {entries.map((entry) => {
          const Icon = entry.icon;
          return (
            <MenuRow key={entry.id} icon={Icon} label={entry.title} value={entry.meta} onClick={() => setSection(entry.id)} />
          );
        })}
      </div>
    </section>
  );
}
