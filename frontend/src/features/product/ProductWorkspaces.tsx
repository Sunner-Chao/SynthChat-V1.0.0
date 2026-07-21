import {
  BookOpen,
  Camera,
  Check,
  ChevronRight,
  Compass,
  type LucideIcon,
} from "lucide-react";
import "./product.css";

export type ProductDestination =
  | "chat"
  | "contacts"
  | "discover"
  | "personas"
  | "moments"
  | "memory"
  | "worldbooks"
  | "plugins"
  | "tools"
  | "skills"
  | "settings";

interface ProductWorkspaceProps {
  onNavigate: (destination: ProductDestination) => void;
}

function ProductMenuRow({
  icon: Icon,
  label,
  meta,
  onClick,
}: {
  icon: LucideIcon;
  label: string;
  meta: string;
  onClick: () => void;
}) {
  return (
    <button className="product-menu-row" onClick={onClick} type="button">
      <span className="product-menu-icon"><Icon aria-hidden="true" size={17} /></span>
      <span className="product-menu-copy"><strong>{label}</strong><small>{meta}</small></span>
      <ChevronRight aria-hidden="true" size={17} />
    </button>
  );
}

export function DiscoverWorkspace({ onNavigate }: ProductWorkspaceProps) {
  return (
    <section className="product-page" aria-label="发现产品面板">
      <header className="product-panel-heading">
        <div className="product-panel-heading-copy">
          <Compass aria-hidden="true" size={18} />
          <div><span>DISCOVER</span><h2>发现</h2></div>
        </div>
        <span className="product-capability-pill is-available" data-capability-state="available">
          <Check aria-hidden="true" size={12} />已接入
        </span>
      </header>
      <div className="product-page-body is-narrow">
        <div className="product-menu-card">
          <ProductMenuRow
            icon={Camera}
            label="朋友圈"
            meta="查看与发布动态"
            onClick={() => onNavigate("moments")}
          />
          <ProductMenuRow
            icon={BookOpen}
            label="世界书"
            meta="管理角色上下文"
            onClick={() => onNavigate("worldbooks")}
          />
        </div>
      </div>
    </section>
  );
}
