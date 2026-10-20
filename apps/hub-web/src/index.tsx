/* @refresh reload */
import { render } from "solid-js/web";
import { Router, Route } from "@solidjs/router";
import "@cell/ui/src/theme.css";
import { Layout } from "./App";
import { Library } from "./pages/Library";
import { GamePage } from "./pages/Game";
import { Servers } from "./pages/Servers";
import { ServerPage } from "./pages/Server";
import { Upload } from "./pages/Upload";
import { Settings } from "./pages/Settings";

render(
  () => (
    <Router root={Layout}>
      <Route path="/" component={Library} />
      <Route path="/games/:id" component={GamePage} />
      <Route path="/servers" component={Servers} />
      <Route path="/servers/:id" component={ServerPage} />
      <Route path="/upload" component={Upload} />
      <Route path="/settings" component={Settings} />
    </Router>
  ),
  document.getElementById("root")!,
);
