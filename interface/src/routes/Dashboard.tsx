import { ActionItemsCard } from "@/components/dashboard/ActionItemsCard";
import { TokenUsageCard } from "@/components/dashboard/TokenUsageCard";
import { ActivityCard } from "@/components/dashboard/ActivityCard";
import { RecentActivityCard } from "@/components/dashboard/RecentActivityCard";


export function Dashboard() {
	return (
		<div className="flex h-full flex-col">
			<div className="min-h-0 flex-1 overflow-y-auto">
				<div className="px-3 py-3 pb-12 md:pl-0 md:pr-3">
					<div className="grid grid-cols-1 gap-3 md:h-[340px] md:grid-cols-2 md:gap-5">
						<ActionItemsCard />
						<TokenUsageCard />
					</div>

					<div className="mt-3 md:mt-5">
						<ActivityCard />
					</div>

					<div className="mt-3 md:mt-5">
						<RecentActivityCard />
					</div>
				</div>
			</div>
		</div>
	);
}
