import { App, Editor, MarkdownView, Modal, Notice, Plugin, PluginSettingTab, Setting, TFile } from 'obsidian';

interface Pdf2MdSettings {
  serviceUrl: string;
  apiKey: string;
  createSeparateNote: boolean;
  detectTables: boolean;
}

const DEFAULT_SETTINGS: Pdf2MdSettings = {
  serviceUrl: 'http://127.0.0.1:3984',
  apiKey: '',
  createSeparateNote: false,
  detectTables: true,
};

export default class Pdf2MdPlugin extends Plugin {
  settings: Pdf2MdSettings = DEFAULT_SETTINGS;

  async onload() {
    await this.loadSettings();

    // 1. Ribbon icon for manual file picker conversion
    this.addRibbonIcon('document', 'Convert PDF to Markdown', () => {
      this.promptPdfConversion();
    });

    // 2. Command Palette: Convert current note attachments or prompt
    this.addCommand({
      id: 'convert-pdf-to-markdown',
      name: 'Convert PDF file to Markdown',
      editorCallback: (editor: Editor, view: MarkdownView) => {
        this.promptPdfConversion(editor);
      },
    });

    // 3. File Context Menu: Right click any PDF file in file explorer
    this.registerEvent(
      this.app.workspace.on('file-menu', (menu, file) => {
        if (file instanceof TFile && file.extension.toLowerCase() === 'pdf') {
          menu.addItem((item) => {
            item
              .setTitle('Convert to Markdown (pdf2md)')
              .setIcon('document')
              .onClick(async () => {
                await this.convertTFile(file);
              });
          });
        }
      })
    );

    // 4. Drag-and-drop listener on active markdown editor
    this.registerEvent(
      this.app.workspace.on('editor-drop', async (evt: DragEvent, editor: Editor, view: MarkdownView) => {
        const files = evt.dataTransfer?.files;
        if (!files || files.length === 0) return;

        for (let i = 0; i < files.length; i++) {
          const file = files[i];
          if (file.name.toLowerCase().endsWith('.pdf')) {
            evt.preventDefault();
            new Notice(`[pdf2md] Converting ${file.name}...`);
            try {
              const buffer = await file.arrayBuffer();
              const md = await this.sendToService(file.name, buffer);
              if (this.settings.createSeparateNote) {
                const noteName = file.name.replace(/\.pdf$/i, '.md');
                await this.app.vault.create(noteName, md);
                new Notice(`[pdf2md] Created ${noteName}!`);
              } else {
                editor.replaceSelection(md);
                new Notice(`[pdf2md] Injected ${file.name} Markdown!`);
              }
            } catch (err: any) {
              new Notice(`[pdf2md] Error: ${err.message}`);
            }
          }
        }
      })
    );

    // 5. Settings Tab
    this.addSettingTab(new Pdf2MdSettingTab(this.app, this));
  }

  async convertTFile(file: TFile) {
    new Notice(`[pdf2md] Reading ${file.name}...`);
    try {
      const buffer = await this.app.vault.readBinary(file);
      const md = await this.sendToService(file.name, buffer);
      const newPath = file.path.replace(/\.pdf$/i, '.md');
      await this.app.vault.create(newPath, md);
      new Notice(`✔ Converted to ${newPath}`);
    } catch (e: any) {
      new Notice(`✖ Conversion failed: ${e.message}`);
    }
  }

  async sendToService(filename: string, data: ArrayBuffer): Promise<string> {
    const url = `${this.settings.serviceUrl.replace(/\/$/, '')}/extract`;
    const formData = new FormData();
    const blob = new Blob([data], { type: 'application/pdf' });
    formData.append('file', blob, filename);

    const headers: Record<string, string> = {};
    if (this.settings.apiKey.trim()) {
      headers['Authorization'] = `Bearer ${this.settings.apiKey.trim()}`;
    }

    const response = await fetch(url, {
      method: 'POST',
      headers,
      body: formData,
    });

    if (!response.ok) {
      const text = await response.text();
      throw new Error(`HTTP ${response.status}: ${text || response.statusText}`);
    }

    return await response.text();
  }

  promptPdfConversion(editor?: Editor) {
    const input = document.createElement('input');
    input.type = 'file';
    input.accept = '.pdf';
    input.onchange = async () => {
      const file = input.files?.[0];
      if (!file) return;
      new Notice(`[pdf2md] Converting ${file.name}...`);
      try {
        const buffer = await file.arrayBuffer();
        const md = await this.sendToService(file.name, buffer);
        if (editor && !this.settings.createSeparateNote) {
          editor.replaceSelection(md);
          new Notice(`✔ Injected into active note!`);
        } else {
          const noteName = file.name.replace(/\.pdf$/i, '.md');
          await this.app.vault.create(noteName, md);
          new Notice(`✔ Saved as ${noteName}`);
        }
      } catch (err: any) {
        new Notice(`✖ Error: ${err.message}`);
      }
    };
    input.click();
  }

  async loadSettings() {
    this.settings = Object.assign({}, DEFAULT_SETTINGS, await this.loadData());
  }

  async saveSettings() {
    await this.saveData(this.settings);
  }
}

class Pdf2MdSettingTab extends PluginSettingTab {
  plugin: Pdf2MdPlugin;

  constructor(app: App, plugin: Pdf2MdPlugin) {
    super(app, plugin);
    this.plugin = plugin;
  }

  display(): void {
    const { containerEl } = this;
    containerEl.empty();
    containerEl.createEl('h2', { text: 'PDF to Markdown Converter Settings' });

    new Setting(containerEl)
      .setName('Converter Service URL')
      .setDesc('Local self-hosted endpoint (default: http://127.0.0.1:3984) or remote SaaS gateway.')
      .addText((text) =>
        text
          .setPlaceholder('http://127.0.0.1:3984')
          .setValue(this.plugin.settings.serviceUrl)
          .onChange(async (value) => {
            this.plugin.settings.serviceUrl = value.trim() || 'http://127.0.0.1:3984';
            await this.plugin.saveSettings();
          })
      );

    new Setting(containerEl)
      .setName('API Key (Optional)')
      .setDesc('Bearer token for remote SaaS authentication or Keycloak-protected gateways.')
      .addText((text) =>
        text
          .setPlaceholder('Bearer / Keycloak token')
          .setValue(this.plugin.settings.apiKey)
          .onChange(async (value) => {
            this.plugin.settings.apiKey = value.trim();
            await this.plugin.saveSettings();
          })
      );

    new Setting(containerEl)
      .setName('Create Separate Note on Drop')
      .setDesc('When dropping a PDF, create a new .md note instead of inserting into the active note.')
      .addToggle((toggle) =>
        toggle
          .setValue(this.plugin.settings.createSeparateNote)
          .onChange(async (value) => {
            this.plugin.settings.createSeparateNote = value;
            await this.plugin.saveSettings();
          })
      );
  }
}
