import base64

path = r'C:\Users\33908\AppData\Roaming\cc.synthchat.v1\synthchat-data\attachments\attachment-ec9b3c0fdaa1486f878921d47466579e-99a53186-af96-40ee-911a-82ad97a658a1.png'
with open(path, 'rb') as f:
    data = base64.b64encode(f.read()).decode()

html = '<html><body style="margin:0;display:flex;justify-content:center;align-items:center;height:100vh;background:#000"><img src="data:image/png;base64,' + data + '" style="max-height:100vh"></body></html>'

out = r'C:\Users\33908\AppData\Roaming\cc.synthchat.v1\synthchat-data\artifacts\view_image2.html'
with open(out, 'w', encoding='utf-8') as f:
    f.write(html)
print('HTML file created:', out)
