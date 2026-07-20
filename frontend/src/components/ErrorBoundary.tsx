import { Component, type ErrorInfo, type ReactNode } from "react";

type Props = {
  children: ReactNode;
  fallback?: ReactNode;
};

type State = {
  error: Error | null;
};

export class ErrorBoundary extends Component<Props, State> {
  state: State = { error: null };

  static getDerivedStateFromError(error: Error): State {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    console.error("React render error caught by ErrorBoundary:", error, info.componentStack);
  }

  reset = () => this.setState({ error: null });

  render() {
    if (this.state.error) {
      if (this.props.fallback) return this.props.fallback;
      return (
        <div style={{ padding: "24px", fontFamily: "monospace", fontSize: "13px" }}>
          <strong>界面渲染错误</strong>
          <pre style={{ marginTop: "8px", whiteSpace: "pre-wrap", wordBreak: "break-word", opacity: 0.7 }}>
            {this.state.error.message}
          </pre>
          <button
            type="button"
            onClick={this.reset}
            style={{ marginTop: "12px", padding: "6px 14px", cursor: "pointer" }}
          >
            重试
          </button>
        </div>
      );
    }
    return this.props.children;
  }
}
