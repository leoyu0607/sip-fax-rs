import { mount } from 'svelte';
import './app.css';
import App from './App.svelte';
import { applyTheme, storedTheme } from './lib/theme';

applyTheme(storedTheme());

const target = document.getElementById('app');
if (target) mount(App, { target });
