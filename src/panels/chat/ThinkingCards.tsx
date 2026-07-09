import { memo, useEffect, useState } from "react";
import { Brain, ChevronRight } from "lucide-react";
import type { ThinkingCard } from "../../lib/messageRenderUtils";

// 清理并格式化模型思考摘要文本
function formatThinkingSummary(summary: string, provider: string): string {
  let text = summary
    .replace(/<!--\s*-->/g, "")  // 清除 HTML 注释占位符
    .trim();

  // OpenAI Responses API 返回英文 markdown 加粗标题，如 "**Planning something**"
  // 将其转为条目列表形式，更易阅读
  if (provider === "openai_responses") {
    text = text
      .replace(/\*\*([^*]+)\*\*/g, (_, content) => `· ${content.trim()}`)
      .replace(/\n{2,}/g, "\n");
  }

  return text.trim();
}

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
    // Only auto-expand when streaming starts — never force-collapse when it ends.
    // Collapsing on streaming=false overrides the user's manual expand action.
    if (card.streaming) setExpanded(true);
  }, [card.streaming, card.key]);
  const providerLabel = card.provider === "anthropic"
    ? "深度思考"
    : card.provider === "openai_responses"
      ? "推理链路"
      : "推理过程";
  const statusLabel = card.streaming ? "思考中" : card.redacted ? "已隐藏" : "思考完成";
  const detail = card.summary
    ? formatThinkingSummary(card.summary, card.provider)
    : card.redacted
      ? "服务商返回了受保护的思考内容，当前仅展示占位，不显示原始链路。"
      : "";
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
          <p style={{ whiteSpace: "pre-line" }}>{detail}</p>
        </div>
      </div>
    </div>
  );
});
