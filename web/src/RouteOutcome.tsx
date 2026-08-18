import type { Component } from "solid-js";

interface Props {
  title: string;
  message: string;
  onRecover?: () => void;
}

const RouteOutcome: Component<Props> = (props) => (
  <section class="route-outcome" role="region" aria-labelledby="route-outcome-title">
    <p class="place-kicker">Machine</p>
    <h1 id="route-outcome-title">{props.title}</h1>
    <p>{props.message}</p>
    {props.onRecover ? (
      <button type="button" onClick={props.onRecover}>Return to Sessions</button>
    ) : null}
  </section>
);

export default RouteOutcome;
