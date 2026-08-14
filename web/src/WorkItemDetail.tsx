// Placeholder for the WorkItemDetail surface (M11). Replaced by the surface itself.
import type { Component } from "solid-js";

const WorkItemDetail: Component<{
  itemRef: string;
  onError?: (message: string) => void;
}> = (props) => (
  <div class="vogt-surface vogt-surface--placeholder">
    <p>{props.itemRef} is not built yet.</p>
  </div>
);

export default WorkItemDetail;
