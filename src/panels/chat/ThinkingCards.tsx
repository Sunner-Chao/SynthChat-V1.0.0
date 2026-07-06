import { memo, useEffect, useState } from "react";
import { Brain, ChevronRight } from "lucide-react";
import type { ThinkingCard } from "../../lib/messageRenderUtils";

export const ThinkingCards = memo(function ThinkingCards({ cards }: { cards: ThinkingCard[] }) {
  return (
    <div className="claw-thinking-card-stack">
      {cards.map((card) => (
        <ThinkingCardView card={card} key={card.key} />
      ))}
    </div>
  );
});

export const ThinkingCardView = memo(function ThinkingCardView({ card }: { card: ThinkingCard }) {
  const [expanded, setExpanded] = useState(card.streaming);
  useEffect(() => {
    setExpanded(card.streaming);
  }, [card.streaming, card.key]);
  const providerLabel = card.provider === "anthropic"
    ? "Anthropic"
    : card.provider === "openai_responses"
      ? "Responses"
      : "Reasoning";
  const statusLabel = card.streaming ? "思考中" : card.redacted ? "已隐藏" : "思考完成";
  const detail = card.summary || (card.redacted ? "服务商返回了受保护的思考内容，当前仅展示占位，不显示原始链路。" : "");
  return (
    <div className={`claw-thinking-card${expanded ? " claw-thinking-card--expanded" : ""}`}>
      <button className="claw-thinking-card-head" onClick={() => setExpanded((value) => !value)} type="button">
        <Brain size={15} />
        <strong>{card.title}</strong>
        <small>{[providerLabel, statusLabel].filter(Boolean).join(" · ")}</small>
        <span className={`claw-tool-chevron${expanded ? " claw-tool-chevron--open" : ""}`}>
          <ChevronRight size={14} />
        </span>
      </button>
      <div className={`claw-thinking-card-body${expanded ? " claw-thinking-card-body--open" : ""}`}>
        <div className="claw-thinking-card-inner">
          <p>{detail}</p>
        </div>
      </div>
    </div>
  );
});
