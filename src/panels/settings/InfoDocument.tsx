import { BackBtn } from "./_shared";

export function InfoDocument({
  onBack,
  title,
  body,
}: {
  onBack?: () => void;
  title: string;
  body: string[];
}) {
  return (
    <div className="primary-panel embedded-panel">
      <div className="panel-title action-title">
        <BackBtn onBack={onBack} />
        <div className="panel-title-text">
          <span>Info</span>
          <strong>{title}</strong>
        </div>
      </div>
      <div className="doc-body">
        {body.map((paragraph) => (
          <p key={paragraph}>{paragraph}</p>
        ))}
      </div>
    </div>
  );
}
