import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { ChevronLeft, ChevronRight } from "lucide-react";
import "./QuickPanel.css";

interface TrayApp {
	name: string;
	path: string;
	tray_ids: string[];
}

interface TrayMenuItem {
	id: number;
	label: string;
	separator: boolean;
	enabled: boolean;
	children: TrayMenuItem[];
}

function trayIdOf(app: TrayApp): string {
	return app.tray_ids[0] ?? app.path;
}

function TrayMenuList({
	items,
	onPick
}: {
	items: TrayMenuItem[];
	onPick: (id: number) => void;
}) {
	return (
		<ul className="qp-tray-menu-list">
			{items.map((item, index) => (
				<TrayMenuRow key={`${item.id}-${index}`} item={item} onPick={onPick} />
			))}
		</ul>
	);
}

function TrayMenuRow({ item, onPick }: { item: TrayMenuItem; onPick: (id: number) => void }) {
	const [open, setOpen] = useState(false);
	if (item.separator) {
		return (
			<li>
				<div className="qp-tray-sep" />
			</li>
		);
	}
	const hasChildren = item.children.length > 0;
	return (
		<li>
			<button
				type="button"
				className="qp-tray-item"
				disabled={!item.enabled && !hasChildren}
				onClick={(e) => {
					e.stopPropagation();
					if (hasChildren) {
						setOpen((value) => !value);
						return;
					}
					if (item.enabled) onPick(item.id);
				}}
			>
				<span>{item.label}</span>
				{hasChildren && <ChevronRight className={open ? "open" : ""} />}
			</button>
			{hasChildren && open && (
				<div className="qp-tray-children">
					<TrayMenuList items={item.children} onPick={onPick} />
				</div>
			)}
		</li>
	);
}

export function TrayPanel({ onBack }: { onBack: () => void }) {
	const [apps, setApps] = useState<TrayApp[] | null>(null);
	const [menuFor, setMenuFor] = useState<string | null>(null);
	const [menu, setMenu] = useState<TrayMenuItem[] | null>(null);

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

	const openApp = (app: TrayApp) => {
		invoke("activate_tray_icon", { trayId: trayIdOf(app) }).catch(() => {});
	};

	const openMenu = (app: TrayApp) => {
		const trayId = trayIdOf(app);
		if (menuFor === trayId) {
			setMenuFor(null);
			setMenu(null);
			return;
		}
		setMenuFor(trayId);
		setMenu(null);
		invoke<TrayMenuItem[]>("get_tray_menu", { trayId })
			.then(setMenu)
			.catch(() => setMenu([]));
	};

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
					{apps.map((app) => {
						const trayId = trayIdOf(app);
						const showing = menuFor === trayId;
						return (
							<li key={trayId} className={showing ? "qp-row expanded" : "qp-row"}>
								<button
									type="button"
									className="qp-row-main"
									onDoubleClick={() => openApp(app)}
									onContextMenu={(e) => {
										e.preventDefault();
										e.stopPropagation();
										openMenu(app);
									}}
								>
									<span className="qp-row-text">
										<span className="qp-row-name">{app.name || app.path}</span>
									</span>
								</button>
								{showing && (
									<div className="qp-tray-menu">
										{menu === null ? (
											<div className="qp-empty">…</div>
										) : menu.length === 0 ? (
											<div className="qp-empty">No options</div>
										) : (
											<TrayMenuList
												items={menu}
												onPick={(itemId) => {
													invoke("activate_tray_menu_item", { trayId, itemId }).catch(() => {});
													setMenuFor(null);
													setMenu(null);
												}}
											/>
										)}
									</div>
								)}
							</li>
						);
					})}
				</ul>
			)}
		</div>
	);
}
