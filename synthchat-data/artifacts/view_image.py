import base64

path = r'C:\Users\33908\AppData\Roaming\cc.synthchat.v1\synthchat-data\attachments\wechat_attachment-3fd804ad549343eaa0fa01d92170679a-wechat-e2e1565a0785@im.bot-media-6c96fa5e2d364547abd9de47aeaeea4a.jpg'
with open(path, 'rb') as f:
    data = base64.b64encode(f.read()).decode()

html = '<html><body style="margin:0;display:flex;justify-content:center;align-items:center;height:100vh;background:#000"><img src="data:image/jpeg;base64,' + data + '" style="max-height:100vh"></body></html>'

out = r'C:\Users\33908\AppData\Roaming\cc.synthchat.v1\synthchat-data\artifacts\view_image.html'
with open(out, 'w', encoding='utf-8') as f:
    f.write(html)
print('HTML file created:', out)
