import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { ChevronLeft } from "lucide-react";
import "./QuickPanel.css";

interface TrayApp {
	name: string;
	path: string;
	tray_ids: string[];
}

export function TrayPanel({ onBack }: { onBack: () => void }) {
	const [apps, setApps] = useState<TrayApp[] | null>(null);

	useEffect(() => {
		let cancelled = false;
		invoke<TrayApp[]>("get_tray_apps")
			.then((items) => {
				if (!cancelled) setApps(items);
			})
			.catch(() => {
				if (!cancelled) setApps([]);
			});
		return () => {
			cancelled = true;
		};
	}, []);

	return (
		<div className="qp" onClick={(e) => e.stopPropagation()} onPointerDown={(e) => e.stopPropagation()}>
			<div className="qp-header">
				<button className="qp-icon-btn" onClick={onBack} aria-label="Back">
					<ChevronLeft />
				</button>
				<span className="qp-title">System Tray</span>
			</div>
			{apps === null ? (
				<div className="qp-empty">…</div>
			) : apps.length === 0 ? (
				<div className="qp-empty">No tray programs</div>
			) : (
				<ul className="qp-list">
					{apps.map((app) => (
						<li key={app.tray_ids[0] ?? app.path} className="qp-row">
							<button
								className="qp-row-main"
								onClick={() => {
									const trayId = app.tray_ids[0] ?? app.path;
									invoke("activate_tray_icon", { trayId }).catch(() => {});
								}}
							>
								<span className="qp-row-text">
									<span className="qp-row-name">{app.name || app.path}</span>
								</span>
							</button>
						</li>
					))}
				</ul>
			)}
		</div>
	);
}
