import ReactMarkdown, { defaultUrlTransform } from "react-markdown";
import remarkGfm from "remark-gfm";

export interface MarkdownContentProps {
  children: string;
  className?: string;
}

function safeUrlTransform(url: string): string {
  const value = url.trim();
  if (/^(?:https?:|mailto:)/iu.test(value)) return defaultUrlTransform(value);
  if (value.startsWith("#") || (value.startsWith("/") && !value.startsWith("//"))) {
    return value;
  }
  return "";
}

export function MarkdownContent({ children, className }: MarkdownContentProps) {
  return (
    <div className={className ? `markdown-content ${className}` : "markdown-content"}>
      <ReactMarkdown
        components={{
          a: ({ children: label, href }) => href ? (
            <a href={href} rel="noreferrer" target="_blank">{label}</a>
          ) : <span>{label}</span>,
          img: ({ alt }) => <span className="markdown-image-placeholder">{alt ?? "图片"}</span>,
        }}
        remarkPlugins={[remarkGfm]}
        skipHtml
        urlTransform={safeUrlTransform}
      >
        {children}
      </ReactMarkdown>
    </div>
  );
}
