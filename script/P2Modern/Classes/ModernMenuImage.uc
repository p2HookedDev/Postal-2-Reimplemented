class ModernMenuImage extends ModernMenuPage;

var localized string    SmoothingNames[5];

function CreateMenuContents()
{
	local UWindowComboControl Combo;
	local int i;

	Super.CreateMenuContents();

	AddTitle(TitleText, TitleFont, TitleAlign);

	bUpdate = false;
	Combo = AddLevelCombo(0, "ModernAntiAliasing", 2, 4);
	for (i = 0; i < ArrayCount(SmoothingNames); i++)
		Combo.AddItem(SmoothingNames[i]);
	AddLevelSlider(1, "ModernBloom", 0, 10, 5);
	AddLevelSlider(2, "ModernSharpen", 0, 10, 4);
	AddBox(3, "ModernRelief");
	AddBox(4, "ModernSpecular");
	AddBox(5, "ModernTessellation");
	AddBack();
	LoadRows();
}

defaultproperties
{
	TitleText="Image Quality"
	BackHelp="Return to the Modern Graphics menu."

	SmoothingNames(0)="Off"
	SmoothingNames(1)="FXAA"
	SmoothingNames(2)="SMAA"
	SmoothingNames(3)="MSAA 2x"
	SmoothingNames(4)="MSAA 4x"

	Labels(0)="Antialiasing"
	Labels(1)="Bloom"
	Labels(2)="Sharpening"
	Labels(3)="Bump mapping"
	Labels(4)="Specular highlights"
	Labels(5)="Tessellation"

	Helps(0)="Smooths jagged edges; FXAA is fastest, SMAA sharper, MSAA costs the most."
	Helps(1)="A glow around bright lights."
	Helps(2)="Makes the picture crisper."
	Helps(3)="Walls and floors catch the light with bumps taken from their textures."
	Helps(4)="Surfaces shine by their material: metal, tile, wood, cloth and skin differ."
	Helps(5)="Rounds the outlines of people and animals."
}
