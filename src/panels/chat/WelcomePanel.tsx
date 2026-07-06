import { memo } from "react";
import { Sparkles } from "lucide-react";

export const WelcomePanel = memo(function WelcomePanel({ disabled, onPrompt }: { disabled: boolean; onPrompt: (text: string) => void }) {
  const prompts = [
    "打开 https://example.com，截图并总结页面内容",
    "联网搜索今天 AI 新闻，整理三条要点",
    "列出当前工作目录的文件，并解释项目结构"
  ];
  return (
    <div className="claw-welcome">
      <div className="claw-welcome-mark"><Sparkles size={28} /></div>
      <h2>今天要让 Agent 做什么？</h2>
      <p>支持 MCP 工具调用、Skills 注入、浏览器/文件任务和多步骤执行图。</p>
      <div>
        {prompts.map((prompt) => (
          <button disabled={disabled} key={prompt} onClick={() => onPrompt(prompt)} type="button">
            {prompt}
          </button>
        ))}
      </div>
    </div>
  );
});
