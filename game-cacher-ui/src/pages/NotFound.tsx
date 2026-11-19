import { Link } from "react-router";
import { EmptyState } from "@/components/ui/empty-state";
import { Button } from "@/components/ui/button";

export default function NotFound() {
  return (
    <EmptyState
      title="Page not found"
      description="That route does not exist."
      action={
        <Button asChild variant="secondary">
          <Link to="/">Back to dashboard</Link>
        </Button>
      }
    />
  );
}
