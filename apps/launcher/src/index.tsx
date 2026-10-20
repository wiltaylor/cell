/* @refresh reload */
import { render } from "solid-js/web";
import { Router, Route } from "@solidjs/router";
import "@cell/ui/src/theme.css";
import "./launcher.css";
import { Layout } from "./App";
import { Library } from "./pages/Library";
import { GamePage } from "./pages/Game";
import { Hub } from "./pages/Hub";
import { Servers } from "./pages/Servers";
import { SettingsPage } from "./pages/Settings";
import { ProfilePage } from "./pages/Profile";
import { Peers } from "./pages/Peers";

render(
  () => (
    <Router root={Layout}>
      <Route path="/" component={Library} />
      <Route path="/game/:id" component={GamePage} />
      <Route path="/hub" component={Hub} />
      <Route path="/servers" component={Servers} />
      <Route path="/settings" component={SettingsPage} />
      <Route path="/profile" component={ProfilePage} />
      <Route path="/peers" component={Peers} />
    </Router>
  ),
  document.getElementById("root")!,
);
